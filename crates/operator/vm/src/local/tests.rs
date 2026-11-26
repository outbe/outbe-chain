use std::{
    collections::BTreeMap,
    fs,
    net::Ipv4Addr,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    sync::{atomic::Ordering, Arc, Mutex},
    thread,
    time::{Duration, Instant},
};

use eyre::Result;
use rustix::fs::{flock, openat, FlockOperation, Mode, OFlags, CWD};
use serde_json::{json, Value};

use super::{recovery::STATUS_DISCOVERY_PAGE_SIZE, *};
use crate::{
    disk::sha256_sparse_file,
    vmm::{calc_app_id, VmInfo, VmmClient, VmmTransport, APP_COMPOSE_JSON},
};

const UUID_OWNED: &str = "550e8400-e29b-41d4-a716-446655440010";
const UUID_FOREIGN: &str = "550e8400-e29b-41d4-a716-446655440099";

fn make_image_and_disk(dir: &Path) -> (PathBuf, PathBuf) {
    let image_dir = dir.join("guest-image");
    fs::create_dir_all(&image_dir).unwrap();
    let digest = "0123456789abcdef".repeat(4);
    fs::write(image_dir.join("metadata.json"), "{}").unwrap();
    fs::write(image_dir.join("digest.txt"), format!("{digest}\n")).unwrap();
    let raw_disk = dir.join("dev.img");
    fs::write(&raw_disk, b"raw-disk-placeholder").unwrap();
    (image_dir, raw_disk)
}

#[derive(Default)]
struct FakeVmmState {
    vms: BTreeMap<String, FakeVm>,
    calls: Vec<String>,
    status_requests: Vec<Value>,
    remove_polls_before_gone: usize,
    drop_create_response_once: bool,
    delay_registration_polls: usize,
    pending_delayed_vm: Option<FakeVm>,
    ignore_shutdown: bool,
    ignore_start: bool,
}

#[derive(Clone)]
struct FakeVm {
    id: String,
    name: String,
    image: String,
    vcpu: u32,
    memory: u32,
    app_id: String,
    status: String,
    running: bool,
    stopped_flag: bool,
    boot_progress: String,
    mac: String,
    annotations: BTreeMap<String, String>,
    remove_polls_left: usize,
}

#[derive(Clone)]
struct StubVmm {
    state: Arc<Mutex<FakeVmmState>>,
}

impl StubVmm {
    fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(FakeVmmState::default())),
        }
    }
}

impl VmmTransport for StubVmm {
    fn prpc_post(&self, method: &str, payload: &[u8]) -> Result<String> {
        let req: Value = serde_json::from_slice(payload)?;
        let mut guard = self.state.lock().unwrap();
        guard.calls.push(method.to_owned());
        match method {
            "Status" => {
                guard.status_requests.push(req.clone());
                // Simulate delayed server-side VM registration appearing after N Status polls.
                if let Some(delayed) = guard.pending_delayed_vm.clone() {
                    if guard.delay_registration_polls == 0 {
                        guard.pending_delayed_vm = None;
                        guard.vms.insert(delayed.id.clone(), delayed);
                    } else {
                        guard.delay_registration_polls -= 1;
                    }
                }

                let brief = req["brief"].as_bool().unwrap_or(false);
                let keyword = req["keyword"].as_str().unwrap_or("");
                let page = req["page"].as_u64().unwrap_or(0) as usize;
                let page_size = req["page_size"].as_u64().unwrap_or(0) as usize;

                let filtered: Vec<&FakeVm> = guard
                    .vms
                    .values()
                    .filter(|vm| {
                        keyword.is_empty()
                            || vm.name.contains(keyword)
                            || vm.id.contains(keyword)
                            || vm.app_id.contains(keyword)
                            || vm.image.contains(keyword)
                    })
                    .collect();
                let total = filtered.len() as u32;
                let page_slice: Vec<&FakeVm> = if page == 0 || page_size == 0 {
                    filtered
                } else {
                    filtered
                        .into_iter()
                        .skip((page - 1) * page_size)
                        .take(page_size)
                        .collect()
                };
                let vms: Vec<Value> = page_slice
                    .into_iter()
                    .map(|vm| {
                        let config = if brief {
                            Value::Null
                        } else {
                            json!({
                                "image": vm.image,
                                "vcpu": vm.vcpu,
                                "memory": vm.memory,
                                "app_id": vm.app_id,
                                "stopped": vm.stopped_flag,
                                "annotations": vm.annotations,
                            })
                        };
                        json!({
                            "id": vm.id,
                            "name": vm.name,
                            "status": vm.status,
                            "running": vm.running,
                            "app_id": vm.app_id,
                            "uptime": if vm.running { "1m 5s" } else { "" },
                            "boot_progress": vm.boot_progress,
                            "configuration": config,
                            "interfaces": [{ "mode": "bridge", "mac": vm.mac }],
                        })
                    })
                    .collect();
                Ok(json!({ "vms": vms, "total": total }).to_string())
            }
            "CreateVm" => {
                let name = req["name"].as_str().unwrap().to_owned();
                let image = req["image"].as_str().unwrap().to_owned();
                let vcpu = req["vcpu"].as_u64().unwrap() as u32;
                let memory = req["memory"].as_u64().unwrap() as u32;
                let app_id = req["app_id"].as_str().unwrap_or("").to_owned();
                let annotations: BTreeMap<String, String> = req["annotations"]
                    .as_object()
                    .map(|m| {
                        m.iter()
                            .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_owned())))
                            .collect()
                    })
                    .unwrap_or_default();
                let id = UUID_OWNED.to_owned();
                let remove_polls_left = guard.remove_polls_before_gone;
                let vm = FakeVm {
                    id: id.clone(),
                    name,
                    image,
                    vcpu,
                    memory,
                    app_id,
                    status: VmInfo::STATUS_STOPPED.into(),
                    running: false,
                    stopped_flag: true,
                    boot_progress: String::new(),
                    mac: "52:54:00:aa:bb:cc".into(),
                    annotations,
                    remove_polls_left,
                };
                if guard.delay_registration_polls > 0 {
                    guard.pending_delayed_vm = Some(vm);
                } else {
                    guard.vms.insert(id.clone(), vm);
                }
                if guard.drop_create_response_once {
                    guard.drop_create_response_once = false;
                    return Err(eyre::eyre!("simulated lost CreateVm response (EOF)"));
                }
                Ok(json!({ "id": id }).to_string())
            }
            "GetInfo" => {
                let id = req["id"].as_str().unwrap();
                let mut remove_now = false;
                let resp = match guard.vms.get_mut(id) {
                    Some(vm) => {
                        if vm.status == VmInfo::STATUS_REMOVING {
                            if vm.remove_polls_left == 0 {
                                remove_now = true;
                                json!({ "found": false, "info": null })
                            } else {
                                vm.remove_polls_left -= 1;
                                json!({
                                    "found": true,
                                    "info": {
                                        "id": vm.id,
                                        "name": vm.name,
                                        "status": vm.status,
                                        "app_id": vm.app_id,
                                        "running": false,
                                        "uptime": "",
                                        "boot_progress": vm.boot_progress,
                                        "configuration": {
                                            "image": vm.image,
                                            "vcpu": vm.vcpu,
                                            "memory": vm.memory,
                                            "app_id": vm.app_id,
                                            "stopped": vm.stopped_flag,
                                            "annotations": vm.annotations,
                                        },
                                        "interfaces": [{ "mode": "bridge", "mac": vm.mac }],
                                    }
                                })
                            }
                        } else {
                            json!({
                                "found": true,
                                "info": {
                                    "id": vm.id,
                                    "name": vm.name,
                                    "status": vm.status,
                                    "app_id": vm.app_id,
                                    "running": vm.running,
                                    "uptime": if vm.running { "1m 5s" } else { "" },
                                    "boot_progress": vm.boot_progress,
                                    "configuration": {
                                        "image": vm.image,
                                        "vcpu": vm.vcpu,
                                        "memory": vm.memory,
                                        "app_id": vm.app_id,
                                        "stopped": vm.stopped_flag,
                                        "annotations": vm.annotations,
                                    },
                                    "interfaces": [{ "mode": "bridge", "mac": vm.mac }],
                                }
                            })
                        }
                    }
                    None => json!({ "found": false, "info": null }),
                };
                if remove_now {
                    guard.vms.remove(id);
                }
                Ok(resp.to_string())
            }
            "StartVm" => {
                let id = req["id"].as_str().unwrap();
                if guard.ignore_start {
                    return Ok("{}".into());
                }
                let vm = guard.vms.get_mut(id).unwrap();
                vm.running = true;
                vm.stopped_flag = false;
                vm.boot_progress = "done".into();
                vm.status = VmInfo::STATUS_RUNNING.into();
                Ok("{}".into())
            }
            "ShutdownVm" => {
                let id = req["id"].as_str().unwrap();
                if guard.ignore_shutdown {
                    return Ok("{}".into());
                }
                let vm = guard.vms.get_mut(id).unwrap();
                vm.running = false;
                // Exact dstack VMM status after graceful ShutdownVm (.started remains true).
                vm.status = VmInfo::STATUS_EXITED.into();
                Ok("{}".into())
            }
            "UpdateVm" => {
                let id = req["id"].as_str().unwrap();
                let vm = guard.vms.get_mut(id).unwrap();
                if let Some(new_img) = req.get("image").and_then(|v| v.as_str()) {
                    vm.image = new_img.to_owned();
                }
                Ok("{}".into())
            }
            "StopVm" => {
                let id = req["id"].as_str().unwrap();
                let vm = guard.vms.get_mut(id).unwrap();
                vm.running = false;
                vm.stopped_flag = true;
                vm.status = VmInfo::STATUS_STOPPED.into();
                Ok("{}".into())
            }
            "RemoveVm" => {
                let id = req["id"].as_str().unwrap();
                let vm = guard.vms.get_mut(id).unwrap();
                vm.running = false;
                vm.status = VmInfo::STATUS_REMOVING.into();
                Ok("{}".into())
            }
            other => Err(eyre::eyre!("unexpected method {other}")),
        }
    }

    fn read_serial_log(&self, vm_id: &str, lines: usize) -> Result<String> {
        Ok(format!("boot line for {vm_id} ({lines})\n"))
    }
}

fn base_opts(dir: &Path, image_dir: &Path, raw_disk: &Path) -> CreateLocalVmOptions {
    let vmm_home = dir.join("vmm");
    CreateLocalVmOptions {
        name: "dev".into(),
        owner_tag: None,
        image_dir: image_dir.to_path_buf(),
        image_name: None,
        flavor: "dev".into(),
        disk_source: raw_disk.to_path_buf(),
        disk_staging_name: None,
        consume_staging: false,
        vcpu: 3,
        memory_gib: 5,
        disk_gib: 10,
        bridge: "obr0".into(),
        app_compose_path: None,
        intended_ip: Some(Ipv4Addr::new(10, 77, 0, 88)),
        no_tee: false,
        vmm_socket: vmm_home.join("run/vmm.sock"),
        vmm_home,
        retry_create: false,
        timeout: None,
        recovery_grace: Duration::from_millis(80),
        poll_interval: Duration::from_millis(10),
    }
}

fn fast_remove_opts() -> RemoveLocalVmOptions {
    RemoveLocalVmOptions {
        force: false,
        forget_intent: false,
        expect_vm_id: None,
        timeout: Duration::from_secs(2),
        recovery_grace: Duration::from_millis(80),
        poll_interval: Duration::from_millis(10),
    }
}

#[test]
fn f1_creating_grace_window_refuses_blind_retry_and_forget_without_explicit_flags() {
    let dir = tempfile::tempdir().unwrap();
    let (image_dir, raw_disk) = make_image_and_disk(dir.path());
    let store = LocalVmStore::new(dir.path().join("state"));
    let stub = StubVmm::new();
    let vmm = VmmClient::new(stub.clone(), "stub");
    let opts = base_opts(dir.path(), &image_dir, &raw_disk);

    // 1. Seed a `Creating` intent whose CreateVm never registered on the server.
    let guard = store.lock_vm("dev").unwrap();
    store
        .save_locked(
            &guard,
            &LocalVmState {
                version: LOCAL_VM_STATE_VERSION,
                name: "dev".into(),
                vm_id: String::new(),
                create_token: Some("b".repeat(32)),
                owner_tag: None,
                vmm_socket: opts.vmm_socket.clone(),
                vmm_home: opts.vmm_home.clone(),
                image_name: "outbe-vm-dev-0123456789abcdef".into(),
                os_image_hash: "0123456789abcdef".repeat(4),
                disk_source: raw_disk.clone(),
                disk_source_sha256: Some(sha256_sparse_file(&raw_disk).unwrap()),
                vcpu: 3,
                memory_gib: 5,
                disk_gib: 10,
                bridge: "obr0".into(),
                intended_ip: Some(Ipv4Addr::new(10, 77, 0, 88)),
                mac: String::new(),
                app_id: calc_app_id(APP_COMPOSE_JSON),
                no_tee: false,
                phase: LocalVmPhase::Creating,
                journal: Vec::new(),
            },
        )
        .unwrap();
    drop(guard);

    // Retry WITHOUT --retry-create must poll grace window, NOT issue CreateVm, and refuse.
    let err = create_local_vm_with_disk_stager(&store, &vmm, &opts, |home, id, _, _| {
        Ok(home.join("vm").join(id).join("hda.img"))
    })
    .unwrap_err();
    assert!(err.to_string().contains("--retry-create"), "{err}");
    assert!(
        !stub
            .state
            .lock()
            .unwrap()
            .calls
            .contains(&"CreateVm".to_owned()),
        "must NOT resend CreateVm without --retry-create"
    );
    // Verify Status discovery used brief=true, keyword="dev", and pagination (item c).
    let status_reqs = stub.state.lock().unwrap().status_requests.clone();
    assert!(!status_reqs.is_empty());
    for sr in &status_reqs {
        assert_eq!(sr["brief"], json!(true));
        assert_eq!(sr["keyword"], json!("dev"));
        assert_eq!(sr["page_size"], json!(STATUS_DISCOVERY_PAGE_SIZE));
    }

    // Remove WITHOUT --forget-intent must NOT delete the Creating intent.
    let err = remove_local_vm(&store, &vmm, "dev", fast_remove_opts()).unwrap_err();
    assert!(err.to_string().contains("--forget-intent"), "{err}");
    assert!(store.state_path("dev").unwrap().exists());

    // Remove WITH --forget-intent deletes the intent cleanly.
    let mut forget_opts = fast_remove_opts();
    forget_opts.forget_intent = true;
    let removed = remove_local_vm(&store, &vmm, "dev", forget_opts).unwrap();
    assert!(removed.forgot_intent);
    assert!(!store.state_path("dev").unwrap().exists());

    // 2. Delayed registration that arrives during the grace window is adopted cleanly with 1 CreateVm.
    let store2 = LocalVmStore::new(dir.path().join("state-delayed"));
    let stub2 = StubVmm::new();
    {
        let mut g = stub2.state.lock().unwrap();
        g.drop_create_response_once = true;
        g.delay_registration_polls = 2;
    }
    let vmm2 = VmmClient::new(stub2.clone(), "stub2");
    assert!(
        create_local_vm_with_disk_stager(&store2, &vmm2, &opts, |home, id, _, _| {
            Ok(home.join("vm").join(id).join("hda.img"))
        })
        .is_err()
    );

    let mut grace_opts = opts.clone();
    grace_opts.recovery_grace = Duration::from_secs(2);
    let recovered =
        create_local_vm_with_disk_stager(&store2, &vmm2, &grace_opts, |home, id, _, _| {
            Ok(home.join("vm").join(id).join("hda.img"))
        })
        .unwrap();
    assert_eq!(recovered.vm_id, UUID_OWNED);
    assert_eq!(
        stub2
            .state
            .lock()
            .unwrap()
            .calls
            .iter()
            .filter(|c| c.as_str() == "CreateVm")
            .count(),
        1,
        "delayed CreateVm must be adopted during grace window without re-issuing CreateVm"
    );

    // 3. Explicit --retry-create when truly absent issues CreateVm and succeeds.
    let store3 = LocalVmStore::new(dir.path().join("state-retry"));
    let stub3 = StubVmm::new();
    let vmm3 = VmmClient::new(stub3.clone(), "stub3");
    let g3 = store3.lock_vm("dev").unwrap();
    store3
        .save_locked(
            &g3,
            &LocalVmState {
                version: LOCAL_VM_STATE_VERSION,
                name: "dev".into(),
                vm_id: String::new(),
                create_token: Some("c".repeat(32)),
                owner_tag: None,
                vmm_socket: opts.vmm_socket.clone(),
                vmm_home: opts.vmm_home.clone(),
                image_name: "outbe-vm-dev-0123456789abcdef".into(),
                os_image_hash: "0123456789abcdef".repeat(4),
                disk_source: raw_disk.clone(),
                disk_source_sha256: Some(sha256_sparse_file(&raw_disk).unwrap()),
                vcpu: 3,
                memory_gib: 5,
                disk_gib: 10,
                bridge: "obr0".into(),
                intended_ip: Some(Ipv4Addr::new(10, 77, 0, 88)),
                mac: String::new(),
                app_id: calc_app_id(APP_COMPOSE_JSON),
                no_tee: false,
                phase: LocalVmPhase::Creating,
                journal: Vec::new(),
            },
        )
        .unwrap();
    drop(g3);
    let mut retry_opts = opts.clone();
    retry_opts.retry_create = true;
    let created =
        create_local_vm_with_disk_stager(&store3, &vmm3, &retry_opts, |home, id, _, _| {
            Ok(home.join("vm").join(id).join("hda.img"))
        })
        .unwrap();
    assert_eq!(created.vm_id, UUID_OWNED);
    assert_eq!(created.phase, LocalVmPhase::Ready);
}

#[test]
fn f1_adoption_refuses_copied_token_with_mismatched_config() {
    let dir = tempfile::tempdir().unwrap();
    let (image_dir, raw_disk) = make_image_and_disk(dir.path());
    let store = LocalVmStore::new(dir.path().join("state"));
    let stub = StubVmm::new();
    let vmm = VmmClient::new(stub.clone(), "stub");
    let opts = base_opts(dir.path(), &image_dir, &raw_disk);

    let token = "d".repeat(32);
    let g = store.lock_vm("dev").unwrap();
    store
        .save_locked(
            &g,
            &LocalVmState {
                version: LOCAL_VM_STATE_VERSION,
                name: "dev".into(),
                vm_id: String::new(),
                create_token: Some(token.clone()),
                owner_tag: None,
                vmm_socket: opts.vmm_socket.clone(),
                vmm_home: opts.vmm_home.clone(),
                image_name: "outbe-vm-dev-0123456789abcdef".into(),
                os_image_hash: "0123456789abcdef".repeat(4),
                disk_source: raw_disk.clone(),
                disk_source_sha256: Some(sha256_sparse_file(&raw_disk).unwrap()),
                vcpu: 3,
                memory_gib: 5,
                disk_gib: 10,
                bridge: "obr0".into(),
                intended_ip: Some(Ipv4Addr::new(10, 77, 0, 88)),
                mac: String::new(),
                app_id: calc_app_id(APP_COMPOSE_JSON),
                no_tee: false,
                phase: LocalVmPhase::Creating,
                journal: Vec::new(),
            },
        )
        .unwrap();
    drop(g);

    // Foreign VM copied our bearer token + name, but has a different image/vcpu/app_id.
    let mut copied_annotations = BTreeMap::new();
    copied_annotations.insert(CREATE_TOKEN_ANNOTATION.to_owned(), token);
    stub.state.lock().unwrap().vms.insert(
        UUID_FOREIGN.into(),
        FakeVm {
            id: UUID_FOREIGN.into(),
            name: "dev".into(),
            image: "evil-other-image".into(),
            vcpu: 8,
            memory: 5 * 1024,
            app_id: "f".repeat(40),
            status: VmInfo::STATUS_STOPPED.into(),
            running: false,
            stopped_flag: true,
            boot_progress: String::new(),
            mac: "52:54:00:de:ad:01".into(),
            annotations: copied_annotations,
            remove_polls_left: 0,
        },
    );

    let err = create_local_vm_with_disk_stager(&store, &vmm, &opts, |_, _, _, _| {
        panic!("must not stage disk onto mismatched adopted VM")
    })
    .unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("refusing to adopt"), "{msg}");
    assert!(msg.contains("image"), "{msg}");
    assert!(msg.contains("vcpu"), "{msg}");
    assert!(msg.contains("app_id"), "{msg}");

    // `remove` must also refuse to adopt and delete a mismatched copied-token VM.
    let err = remove_local_vm(&store, &vmm, "dev", fast_remove_opts()).unwrap_err();
    assert!(err.to_string().contains("refusing to adopt"), "{err}");
}

#[test]
fn f1_lost_create_response_recovers_via_token_and_never_adopts_foreign_vm() {
    let dir = tempfile::tempdir().unwrap();
    let (image_dir, raw_disk) = make_image_and_disk(dir.path());
    let store = LocalVmStore::new(dir.path().join("state"));
    let stub = StubVmm::new();
    stub.state.lock().unwrap().drop_create_response_once = true;
    let vmm = VmmClient::new(stub.clone(), "stub");
    let opts = base_opts(dir.path(), &image_dir, &raw_disk);

    // Initial CreateVm registers VM on server, then connection drops before response.
    let err = create_local_vm_with_disk_stager(&store, &vmm, &opts, |home, id, _, _| {
        Ok(home.join("vm").join(id).join("hda.img"))
    })
    .unwrap_err();
    let msg = format!("{err:#}");
    assert!(msg.contains("simulated lost CreateVm response"), "{msg}");

    // Durable `Creating` state file with create_token already exists on disk.
    let guard = store.lock_vm("dev").unwrap();
    let creating = store.load_locked(&guard, "dev").unwrap();
    assert_eq!(creating.phase, LocalVmPhase::Creating);
    assert!(creating.vm_id.is_empty());
    assert!(creating.create_token.is_some());
    drop(guard);

    // Retrying `create` finds our exact VM by CREATE_TOKEN_ANNOTATION (no second CreateVm)
    // and finishes cleanly.
    let calls_before = stub.state.lock().unwrap().calls.len();
    let created = create_local_vm_with_disk_stager(&store, &vmm, &opts, |home, id, _, _| {
        Ok(home.join("vm").join(id).join("hda.img"))
    })
    .unwrap();
    assert_eq!(created.vm_id, UUID_OWNED);
    assert_eq!(created.phase, LocalVmPhase::Ready);
    let later_calls = &stub.state.lock().unwrap().calls[calls_before..];
    assert!(
        !later_calls.contains(&"CreateVm".to_owned()),
        "retry must adopt token-matched VM instead of calling CreateVm twice: {later_calls:?}"
    );

    // Also verify `remove` cleans up an interrupted `Creating` VM.
    let store2 = LocalVmStore::new(dir.path().join("state-2"));
    let stub2 = StubVmm::new();
    stub2.state.lock().unwrap().drop_create_response_once = true;
    let vmm2 = VmmClient::new(stub2.clone(), "stub2");
    assert!(
        create_local_vm_with_disk_stager(&store2, &vmm2, &opts, |home, id, _, _| {
            Ok(home.join("vm").join(id).join("hda.img"))
        })
        .is_err()
    );
    let removed = remove_local_vm(&store2, &vmm2, "dev", fast_remove_opts()).unwrap();
    assert_eq!(removed.vm_id, UUID_OWNED);
    assert!(stub2.state.lock().unwrap().vms.is_empty());

    // Foreign VM with the same name WITHOUT our token is refused even with --retry-create.
    let store3 = LocalVmStore::new(dir.path().join("state-3"));
    let stub3 = StubVmm::new();
    stub3.state.lock().unwrap().vms.insert(
        UUID_FOREIGN.into(),
        FakeVm {
            id: UUID_FOREIGN.into(),
            name: "dev".into(),
            image: "foreign".into(),
            vcpu: 3,
            memory: 5 * 1024,
            app_id: calc_app_id(APP_COMPOSE_JSON),
            status: VmInfo::STATUS_STOPPED.into(),
            running: false,
            stopped_flag: true,
            boot_progress: String::new(),
            mac: "52:54:00:de:ad:01".into(),
            annotations: BTreeMap::new(),
            remove_polls_left: 0,
        },
    );
    let guard3 = store3.lock_vm("dev").unwrap();
    store3
        .save_locked(
            &guard3,
            &LocalVmState {
                version: LOCAL_VM_STATE_VERSION,
                name: "dev".into(),
                vm_id: String::new(),
                create_token: Some("a".repeat(32)),
                owner_tag: None,
                vmm_socket: opts.vmm_socket.clone(),
                vmm_home: opts.vmm_home.clone(),
                image_name: "outbe-vm-dev-0123456789abcdef".into(),
                os_image_hash: "0123456789abcdef".repeat(4),
                disk_source: raw_disk.clone(),
                disk_source_sha256: Some(sha256_sparse_file(&raw_disk).unwrap()),
                vcpu: 3,
                memory_gib: 5,
                disk_gib: 10,
                bridge: "obr0".into(),
                intended_ip: Some(Ipv4Addr::new(10, 77, 0, 88)),
                mac: String::new(),
                app_id: calc_app_id(APP_COMPOSE_JSON),
                no_tee: false,
                phase: LocalVmPhase::Creating,
                journal: Vec::new(),
            },
        )
        .unwrap();
    drop(guard3);
    let vmm3 = VmmClient::new(stub3.clone(), "stub3");
    let mut retry_opts = opts.clone();
    retry_opts.retry_create = true;
    let err = create_local_vm_with_disk_stager(&store3, &vmm3, &retry_opts, |home, id, _, _| {
        Ok(home.join("vm").join(id).join("hda.img"))
    })
    .unwrap_err();
    assert!(
        err.to_string().contains("already exists on the local VMM"),
        "must refuse foreign same-name VM without matching token: {err}"
    );
}

#[test]
fn f8_cancel_aborts_during_lock_wait_without_issuing_vmm_rpcs() {
    let dir = tempfile::tempdir().unwrap();
    let (image_dir, raw_disk) = make_image_and_disk(dir.path());
    let store = LocalVmStore::new(dir.path().join("state"));
    let stub = StubVmm::new();
    let vmm = VmmClient::new(stub.clone(), "stub");
    let opts = base_opts(dir.path(), &image_dir, &raw_disk);

    // Hold the per-VM lock externally.
    let held = store.lock_vm("dev").unwrap();
    let cancel = Arc::new(AtomicBool::new(false));
    let cancel_bg = Arc::clone(&cancel);
    let trigger = thread::spawn(move || {
        thread::sleep(Duration::from_millis(60));
        cancel_bg.store(true, Ordering::SeqCst);
    });

    let started = Instant::now();
    let err = create_local_vm_with_disk_stager_cancellable(
        &store,
        &vmm,
        &opts,
        Some(&cancel),
        |_, _, _, _| panic!("disk stager must not run after cancel"),
    )
    .unwrap_err();
    let elapsed = started.elapsed();
    trigger.join().unwrap();
    drop(held);

    assert!(
        elapsed < Duration::from_secs(1),
        "lock wait did not honor cancellation promptly: {elapsed:?}"
    );
    assert!(err.to_string().contains("interrupted by signal"), "{err}");
    assert!(
        stub.state.lock().unwrap().calls.is_empty(),
        "no VMM RPCs may be sent after pre-lock cancel, saw: {:?}",
        stub.state.lock().unwrap().calls
    );
}

#[test]
fn f2_and_f3_resumed_create_refuses_started_vm_or_changed_immutable_inputs() {
    let dir = tempfile::tempdir().unwrap();
    let (image_dir, raw_disk) = make_image_and_disk(dir.path());
    let store = LocalVmStore::new(dir.path().join("state"));
    let stub = StubVmm::new();
    let vmm = VmmClient::new(stub.clone(), "stub");
    let opts = base_opts(dir.path(), &image_dir, &raw_disk);

    // Fail during disk staging to leave a MacRecorded partial state.
    assert!(
        create_local_vm_with_disk_stager(&store, &vmm, &opts, |_, _, _, _| {
            Err(eyre::eyre!("simulated stage failure"))
        })
        .is_err()
    );

    // F3: retry with a different --disk path must be refused.
    let other_disk = dir.path().join("other.img");
    fs::write(&other_disk, b"different-raw-disk-contents").unwrap();
    let mut diff_disk_opts = opts.clone();
    diff_disk_opts.disk_source = other_disk;
    let err = create_local_vm_with_disk_stager(&store, &vmm, &diff_disk_opts, |_, _, _, _| {
        panic!("disk stager must not run on mismatched disk")
    })
    .unwrap_err();
    assert!(err.to_string().contains("disk_source"), "{err}");

    // F3: mutating bytes at the SAME --disk path must be refused via recorded SHA-256.
    let original_bytes = fs::read(&raw_disk).unwrap();
    fs::write(&raw_disk, b"mutated-raw-disk-bytes").unwrap();
    let err = create_local_vm_with_disk_stager(&store, &vmm, &opts, |_, _, _, _| {
        panic!("disk stager must not run when raw disk sha256 changed")
    })
    .unwrap_err();
    assert!(err.to_string().contains("disk_source_sha256"), "{err}");
    fs::write(&raw_disk, original_bytes).unwrap();

    // F3: retry with a different --ip must be refused.
    let mut diff_ip_opts = opts.clone();
    diff_ip_opts.intended_ip = Some(Ipv4Addr::new(10, 77, 0, 99));
    let err = create_local_vm_with_disk_stager(&store, &vmm, &diff_ip_opts, |_, _, _, _| {
        panic!("disk stager must not run on mismatched intended_ip")
    })
    .unwrap_err();
    assert!(err.to_string().contains("intended_ip"), "{err}");

    // F2: external start before retry must refuse staging hda.img.
    {
        let mut g = stub.state.lock().unwrap();
        let vm = g.vms.get_mut(UUID_OWNED).unwrap();
        vm.running = true;
        vm.stopped_flag = false;
        vm.status = VmInfo::STATUS_RUNNING.into();
    }
    let err = create_local_vm_with_disk_stager(&store, &vmm, &opts, |_, _, _, _| {
        panic!("disk stager must not run on a running VM")
    })
    .unwrap_err();
    assert!(
        err.to_string().contains("refusing to stage hda.img"),
        "{err}"
    );

    // F2: even if stopped again after booting (`boot_progress` set), refuse staging hda.img.
    {
        let mut g = stub.state.lock().unwrap();
        let vm = g.vms.get_mut(UUID_OWNED).unwrap();
        vm.running = false;
        vm.stopped_flag = true;
        vm.status = VmInfo::STATUS_STOPPED.into();
        vm.boot_progress = "done".into();
    }
    let err = create_local_vm_with_disk_stager(&store, &vmm, &opts, |_, _, _, _| {
        panic!("disk stager must not run on a previously booted VM")
    })
    .unwrap_err();
    assert!(err.to_string().contains("already booted"), "{err}");
}

#[test]
fn f6_force_stop_on_exited_vm_sends_stop_vm_to_clear_started_flag() {
    let dir = tempfile::tempdir().unwrap();
    let (image_dir, raw_disk) = make_image_and_disk(dir.path());
    let store = LocalVmStore::new(dir.path().join("state"));
    let stub = StubVmm::new();
    let vmm = VmmClient::new(stub.clone(), "stub");
    let opts = base_opts(dir.path(), &image_dir, &raw_disk);

    create_local_vm_with_disk_stager(&store, &vmm, &opts, |home, id, _, _| {
        Ok(home.join("vm").join(id).join("hda.img"))
    })
    .unwrap();
    start_local_vm(&store, &vmm, "dev").unwrap();

    // Graceful stop moves VM to `exited` (.started remains true).
    let graceful = stop_local_vm(
        &store,
        &vmm,
        "dev",
        StopLocalVmOptions {
            force: false,
            timeout: Duration::from_secs(2),
            poll_interval: Duration::from_millis(10),
        },
    )
    .unwrap();
    assert_eq!(graceful.vmm_status, VmInfo::STATUS_EXITED);
    assert!(!graceful.forced);
    assert!(!graceful.cleared_started_flag);

    // Now `stop --force` on the `exited` VM MUST send `StopVm` to clear `.started`.
    let cleared = stop_local_vm(
        &store,
        &vmm,
        "dev",
        StopLocalVmOptions {
            force: true,
            timeout: Duration::from_secs(2),
            poll_interval: Duration::from_millis(10),
        },
    )
    .unwrap();
    assert!(!cleared.already_stopped);
    assert!(cleared.forced);
    assert!(cleared.cleared_started_flag);
    assert_eq!(cleared.vmm_status, VmInfo::STATUS_STOPPED);
    assert!(stub
        .state
        .lock()
        .unwrap()
        .calls
        .contains(&"StopVm".to_owned()));

    // A second `stop --force` once already administratively `stopped` is a no-op.
    let noop = stop_local_vm(
        &store,
        &vmm,
        "dev",
        StopLocalVmOptions {
            force: true,
            timeout: Duration::from_secs(2),
            poll_interval: Duration::from_millis(10),
        },
    )
    .unwrap();
    assert!(noop.already_stopped);
    assert!(!noop.forced);
    assert_eq!(noop.vmm_status, VmInfo::STATUS_STOPPED);
}

#[test]
fn f7_unsafe_preexisting_state_permissions_are_refused_unless_fix_permissions() {
    let dir = tempfile::tempdir().unwrap();
    let (image_dir, raw_disk) = make_image_and_disk(dir.path());
    let state_dir = dir.path().join("state");
    let store = LocalVmStore::new(&state_dir);
    let stub = StubVmm::new();
    let vmm = VmmClient::new(stub, "stub");
    let opts = base_opts(dir.path(), &image_dir, &raw_disk);

    create_local_vm_with_disk_stager(&store, &vmm, &opts, |home, id, _, _| {
        Ok(home.join("vm").join(id).join("hda.img"))
    })
    .unwrap();

    let vms_dir = store.vms_dir();
    let state_file = store.state_path("dev").unwrap();
    let leases = dir.path().join("dhcp-leases");

    // Group/world-writable state directory must be refused by default.
    fs::set_permissions(&state_dir, fs::Permissions::from_mode(0o777)).unwrap();
    let err = status_local_vm(&store, &vmm, "dev", &leases).unwrap_err();
    assert!(err.to_string().contains("unsafe mode"), "{err}");

    // Group/world-writable state file must also be refused by default.
    fs::set_permissions(&state_dir, fs::Permissions::from_mode(0o700)).unwrap();
    fs::set_permissions(&state_file, fs::Permissions::from_mode(0o666)).unwrap();
    let err = status_local_vm(&store, &vmm, "dev", &leases).unwrap_err();
    assert!(err.to_string().contains("unsafe mode"), "{err}");

    // With explicit --fix-permissions, owned unsafe paths are tightened to 0700 / 0600.
    fs::set_permissions(&state_dir, fs::Permissions::from_mode(0o777)).unwrap();
    fs::set_permissions(&vms_dir, fs::Permissions::from_mode(0o777)).unwrap();
    fs::set_permissions(&state_file, fs::Permissions::from_mode(0o666)).unwrap();
    let fixing_store = LocalVmStore::new(&state_dir).with_fix_permissions(true);
    status_local_vm(&fixing_store, &vmm, "dev", &leases).unwrap();
    assert_eq!(
        fs::metadata(&state_dir).unwrap().permissions().mode() & 0o777,
        0o700
    );
    assert_eq!(
        fs::metadata(&vms_dir).unwrap().permissions().mode() & 0o777,
        0o700
    );
    assert_eq!(
        fs::metadata(&state_file).unwrap().permissions().mode() & 0o777,
        0o600
    );
}

#[test]
fn state_journal_permissions_refusals_and_force_warning() {
    let dir = tempfile::tempdir().unwrap();
    let (image_dir, raw_disk) = make_image_and_disk(dir.path());
    let state_dir = dir.path().join("state");
    let vmm_home = dir.path().join("vmm");
    let store = LocalVmStore::new(&state_dir);
    let stub = StubVmm::new();
    stub.state.lock().unwrap().remove_polls_before_gone = 2;
    let vmm = VmmClient::new(stub.clone(), "stub");

    // Refusal: unowned VM cannot be started, stopped, inspected, or removed.
    assert!(start_local_vm(&store, &vmm, "foreign").is_err());
    assert!(stop_local_vm(
        &store,
        &vmm,
        "foreign",
        StopLocalVmOptions {
            force: false,
            timeout: Duration::from_secs(1),
            poll_interval: Duration::from_millis(10)
        }
    )
    .is_err());
    assert!(remove_local_vm(&store, &vmm, "foreign", fast_remove_opts()).is_err());
    assert!(logs_local_vm(&store, &vmm, "foreign", 20).is_err());

    // Refusal: name collision with foreign VM on VMM before CreateVm.
    stub.state.lock().unwrap().vms.insert(
        UUID_FOREIGN.into(),
        FakeVm {
            id: UUID_FOREIGN.into(),
            name: "dev".into(),
            image: "other".into(),
            vcpu: 3,
            memory: 5 * 1024,
            app_id: calc_app_id(APP_COMPOSE_JSON),
            status: VmInfo::STATUS_RUNNING.into(),
            running: true,
            stopped_flag: false,
            boot_progress: "done".into(),
            mac: "52:54:00:de:ad:01".into(),
            annotations: BTreeMap::new(),
            remove_polls_left: 0,
        },
    );
    let opts = base_opts(dir.path(), &image_dir, &raw_disk);
    let err = create_local_vm_with_disk_stager(&store, &vmm, &opts, |home, id, _, _| {
        Ok(home.join("vm").join(id).join("hda.img"))
    })
    .unwrap_err();
    assert!(err.to_string().contains("already exists on the local VMM"));
    stub.state.lock().unwrap().vms.remove(UUID_FOREIGN);

    // Partial failure during disk staging leaves journaled MacRecorded state and resumes cleanly.
    let err = create_local_vm_with_disk_stager(&store, &vmm, &opts, |_, _, _, _| {
        Err(eyre::eyre!("simulated qemu-img failure"))
    })
    .unwrap_err();
    assert!(err.to_string().contains("simulated qemu-img failure"));
    let guard = store.lock_vm("dev").unwrap();
    let partial = store.load_locked(&guard, "dev").unwrap();
    assert_eq!(partial.phase, LocalVmPhase::MacRecorded);
    assert_eq!(partial.vm_id, UUID_OWNED);
    assert_eq!(partial.mac, "52:54:00:aa:bb:cc");
    assert!(partial.journal.iter().any(|j| j.step == "staging-disk"));
    // Verify 0700 dir and 0600 state file permissions.
    assert_eq!(
        fs::metadata(&state_dir).unwrap().permissions().mode() & 0o777,
        0o700
    );
    assert_eq!(
        fs::metadata(store.state_path("dev").unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    drop(guard);

    // Starting an incomplete VM is refused.
    assert!(start_local_vm(&store, &vmm, "dev").is_err());

    // Resume finishes disk staging and reaches Ready.
    let created = create_local_vm_with_disk_stager(&store, &vmm, &opts, |home, id, _, _| {
        let p = home.join("vm").join(id).join("hda.img");
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(&p, b"qcow2").unwrap();
        Ok(p)
    })
    .unwrap();
    assert_eq!(created.phase, LocalVmPhase::Ready);
    assert_eq!(created.intended_ip, Some(Ipv4Addr::new(10, 77, 0, 88)));

    // Start, graceful stop (uses ShutdownVm, NOT StopVm), start again, force stop (prints warning).
    let started = start_local_vm(&store, &vmm, "dev").unwrap();
    assert!(!started.already_running);
    let stopped = stop_local_vm(
        &store,
        &vmm,
        "dev",
        StopLocalVmOptions {
            force: false,
            timeout: Duration::from_secs(2),
            poll_interval: Duration::from_millis(10),
        },
    )
    .unwrap();
    assert!(!stopped.forced);
    assert_eq!(stopped.vmm_status, VmInfo::STATUS_EXITED);
    assert!(stopped.warning.is_none());
    assert!(!stub
        .state
        .lock()
        .unwrap()
        .calls
        .contains(&"StopVm".to_owned()));

    // Force stop calls StopVm and returns the DKG defect warning.
    start_local_vm(&store, &vmm, "dev").unwrap();
    let forced = stop_local_vm(
        &store,
        &vmm,
        "dev",
        StopLocalVmOptions {
            force: true,
            timeout: Duration::from_secs(2),
            poll_interval: Duration::from_millis(10),
        },
    )
    .unwrap();
    assert!(forced.forced);
    assert_eq!(forced.vmm_status, VmInfo::STATUS_STOPPED);
    assert!(forced
        .warning
        .as_deref()
        .unwrap()
        .contains("12-defect-pending-dkg-snapshot.md"));

    // Status reports both service_ip (recorded intended_ip) and dhcp_ip (from leases file,
    // None when absent).
    let leases_file = dir.path().join("dhcp-leases");
    let status_no_leases = status_local_vm(&store, &vmm, "dev", &leases_file).unwrap();
    assert_eq!(
        status_no_leases.service_ip,
        Some(Ipv4Addr::new(10, 77, 0, 88))
    );
    assert_eq!(status_no_leases.dhcp_ip, None);
    assert_eq!(status_no_leases.ip, Some(Ipv4Addr::new(10, 77, 0, 88)));
    fs::write(
        &leases_file,
        "1795598734 52:54:00:aa:bb:cc 10.77.0.122 dev *\n",
    )
    .unwrap();
    let status_with_leases = status_local_vm(&store, &vmm, "dev", &leases_file).unwrap();
    assert_eq!(
        status_with_leases.service_ip,
        Some(Ipv4Addr::new(10, 77, 0, 88))
    );
    assert_eq!(
        status_with_leases.dhcp_ip,
        Some(Ipv4Addr::new(10, 77, 0, 122))
    );
    assert_eq!(status_with_leases.ip, Some(Ipv4Addr::new(10, 77, 0, 88)));

    // Remove waits across asynchronous polls and deletes only state file, leaving image intact.
    let removed = remove_local_vm(&store, &vmm, "dev", fast_remove_opts()).unwrap();
    assert_eq!(removed.vm_id, UUID_OWNED);
    assert!(!store.state_path("dev").unwrap().exists());
    assert!(vmm_home.join("image").join(&created.image_name).is_dir());
}

#[test]
fn concurrent_vm_state_locks_serialize_without_corrupting_state() {
    let dir = tempfile::tempdir().unwrap();
    let store = LocalVmStore::new(dir.path().join("state"));

    // Seed initial state.
    let guard = store.lock_vm("race-vm").unwrap();
    let initial = LocalVmState {
        version: LOCAL_VM_STATE_VERSION,
        name: "race-vm".into(),
        vm_id: UUID_OWNED.into(),
        create_token: None,
        owner_tag: None,
        vmm_socket: PathBuf::from("/tmp/vmm.sock"),
        vmm_home: PathBuf::from("/tmp/vmm"),
        image_name: "outbe-vm-dev-0123456789abcdef".into(),
        os_image_hash: "0123456789abcdef".repeat(4),
        disk_source: PathBuf::from("/tmp/dev.img"),
        disk_source_sha256: Some("0".repeat(64)),
        vcpu: 3,
        memory_gib: 5,
        disk_gib: 10,
        bridge: "obr0".into(),
        intended_ip: None,
        mac: "52:54:00:11:22:33".into(),
        app_id: "a".repeat(40),
        no_tee: false,
        phase: LocalVmPhase::Ready,
        journal: Vec::new(),
    };
    store.save_locked(&guard, &initial).unwrap();
    drop(guard);

    let mut handles = Vec::new();
    for worker in 0..4 {
        let store = store.clone();
        handles.push(thread::spawn(move || {
            for iter in 0..8 {
                let g = store.lock_vm("race-vm").unwrap();
                let mut st = store.load_locked(&g, "race-vm").unwrap();
                st.record_step(format!("w{worker}-i{iter}"), None);
                store.save_locked(&g, &st).unwrap();
            }
        }));
    }
    for h in handles {
        h.join().unwrap();
    }

    let g = store.lock_vm("race-vm").unwrap();
    let final_state = store.load_locked(&g, "race-vm").unwrap();
    assert_eq!(final_state.journal.len(), 32);
}

#[test]
fn f5_remove_local_image_waits_for_vm_create_lock_and_refuses_registered_vm() {
    use std::sync::{mpsc, Arc, Mutex};

    use crate::image::{publish_local_image, remove_local_image};

    struct PausingCreateTransport {
        vms: Arc<Mutex<Vec<serde_json::Value>>>,
        create_entered_tx: Mutex<Option<mpsc::Sender<()>>>,
        allow_create_rx: Mutex<Option<mpsc::Receiver<()>>>,
    }

    impl VmmTransport for PausingCreateTransport {
        fn prpc_post(&self, method: &str, payload: &[u8]) -> Result<String> {
            match method {
                "Status" => {
                    let vms = self.vms.lock().unwrap();
                    Ok(serde_json::json!({
                        "vms": *vms,
                        "total": vms.len()
                    })
                    .to_string())
                }
                "CreateVm" => {
                    let req: serde_json::Value = serde_json::from_slice(payload)?;
                    if let Some(tx) = self.create_entered_tx.lock().unwrap().take() {
                        let _ = tx.send(());
                    }
                    if let Some(rx) = self.allow_create_rx.lock().unwrap().take() {
                        let _ = rx.recv();
                    }
                    let info = serde_json::json!({
                        "id": UUID_OWNED,
                        "name": req["name"].as_str().unwrap_or("race-create"),
                        "status": "stopped",
                        "running": false,
                        "boot_progress": "",
                        "boot_error": "",
                        "uptime": "",
                        "interfaces": [
                            {"mode": "bridge", "mac": "52:54:00:ab:cd:ef"}
                        ],
                        "configuration": {
                            "name": req["name"].as_str().unwrap_or("race-create"),
                            "image": req["image"].as_str().unwrap_or(""),
                            "vcpu": req["vcpu"].as_u64().unwrap_or(3),
                            "memory": req["memory"].as_u64().unwrap_or(5120),
                            "disk_size": req["disk_size"].as_u64().unwrap_or(40)
                        }
                    });
                    self.vms.lock().unwrap().push(info);
                    Ok(serde_json::json!({"id": UUID_OWNED}).to_string())
                }
                "GetInfo" => {
                    let vms = self.vms.lock().unwrap();
                    if let Some(info) = vms.first() {
                        Ok(serde_json::json!({"found": true, "info": info}).to_string())
                    } else {
                        Ok(r#"{"found":false,"info":null}"#.to_owned())
                    }
                }
                other => Err(eyre::eyre!("unexpected VMM method {other}")),
            }
        }

        fn read_serial_log(&self, _vm_id: &str, _lines: usize) -> Result<String> {
            Ok(String::new())
        }
    }

    let dir = tempfile::tempdir().unwrap();
    let vmm_home = dir.path().join("vmm");
    let state_dir = dir.path().join("state");
    let src_image = dir.path().join("src-image");
    fs::create_dir_all(&src_image).unwrap();
    let digest = "0123456789abcdef".repeat(4);
    fs::write(src_image.join("digest.txt"), format!("{digest}\n")).unwrap();
    fs::write(src_image.join("metadata.json"), "{}").unwrap();
    fs::write(
        src_image.join("image-archive-manifest.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "version": 1,
            "flavor": "dev",
            "os_image_hash": digest,
            "source_commit": "a".repeat(40),
            "dstack_rev": "b".repeat(40),
            "overlay_commit": "c".repeat(40),
            "files": [
                {"path": "digest.txt", "size": 65, "sha256": "0".repeat(64), "mode": 0o644},
                {"path": "metadata.json", "size": 2, "sha256": "1".repeat(64), "mode": 0o644}
            ]
        }))
        .unwrap(),
    )
    .unwrap();

    // Pre-publish the image so a concurrent `image remove` without F5 locking would delete it
    // while `CreateVm` is paused before registering the VM.
    let published = publish_local_image(&vmm_home, &src_image, None, "dev").unwrap();
    let disk_source = dir.path().join("data.raw");
    fs::write(&disk_source, b"fake-raw-disk").unwrap();

    let shared_vms = Arc::new(Mutex::new(Vec::new()));
    let (create_entered_tx, create_entered_rx) = mpsc::channel();
    let (allow_create_tx, allow_create_rx) = mpsc::channel();

    let create_transport = PausingCreateTransport {
        vms: Arc::clone(&shared_vms),
        create_entered_tx: Mutex::new(Some(create_entered_tx)),
        allow_create_rx: Mutex::new(Some(allow_create_rx)),
    };
    let remove_transport = PausingCreateTransport {
        vms: Arc::clone(&shared_vms),
        create_entered_tx: Mutex::new(None),
        allow_create_rx: Mutex::new(None),
    };

    let store = LocalVmStore::new(state_dir);
    let opts = CreateLocalVmOptions {
        name: "race-vm".to_owned(),
        owner_tag: None,
        image_dir: src_image,
        image_name: None,
        flavor: "dev".to_owned(),
        disk_source,
        disk_staging_name: None,
        consume_staging: false,
        bridge: "obr0".to_owned(),
        vcpu: 3,
        memory_gib: 5,
        disk_gib: 10,
        app_compose_path: None,
        intended_ip: None,
        no_tee: false,
        vmm_socket: vmm_home.join("run/vmm.sock"),
        vmm_home: vmm_home.clone(),
        retry_create: false,
        recovery_grace: Duration::from_secs(1),
        poll_interval: Duration::from_millis(10),
        timeout: Some(Duration::from_secs(10)),
    };

    let create_handle = thread::spawn(move || {
        let vmm = VmmClient::new(create_transport, "pausing-create");
        create_local_vm_with_disk_stager(&store, &vmm, &opts, |home, id, _, _| {
            let p = home.join("vm").join(id).join("hda.img");
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(&p, b"qcow2").unwrap();
            Ok(p)
        })
    });

    // Wait until `vm create` is inside `CreateVm` (still holding the per-image lock).
    create_entered_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("vm create did not reach CreateVm");

    let (remove_done_tx, remove_done_rx) = mpsc::channel();
    let vmm_home_for_rm = vmm_home.clone();
    let image_name_for_rm = published.name.clone();
    let remove_handle = thread::spawn(move || {
        let vmm = VmmClient::new(remove_transport, "concurrent-remove");
        let res = remove_local_image(&vmm_home_for_rm, &vmm, &image_name_for_rm);
        let _ = remove_done_tx.send(res);
    });

    // While CreateVm is paused, `image remove` MUST stay blocked on the per-image lock.
    assert!(
        matches!(
            remove_done_rx.recv_timeout(Duration::from_millis(200)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ),
        "image remove must block while vm create holds the per-image lock across CreateVm"
    );

    // Allow CreateVm to register the VM and finish persisting `Created`.
    allow_create_tx.send(()).unwrap();
    let created = create_handle.join().unwrap().expect("vm create failed");
    assert_eq!(created.phase, LocalVmPhase::Ready);

    let remove_err = remove_done_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("image remove did not finish after CreateVm released lock")
        .expect_err("image remove must refuse image referenced by newly created VM");
    remove_handle.join().unwrap();
    assert!(
        format!("{remove_err:#}").contains("still referenced by VM(s): race-vm"),
        "{remove_err:#}"
    );
    assert!(
        published.target_dir.is_dir(),
        "referenced image directory must remain intact"
    );
}

#[test]
fn c1_owner_tag_persists_in_state_vmm_annotation_status_and_adoption_check() {
    let dir = tempfile::tempdir().unwrap();
    let (image_dir, raw_disk) = make_image_and_disk(dir.path());
    let store = LocalVmStore::new(dir.path().join("state"));
    let stub = StubVmm::new();
    let vmm = VmmClient::new(stub.clone(), "stub");
    let tag = "outbe-net:devnet:01234567:validator-0".to_owned();

    // Invalid owner tags are rejected before any VMM RPC.
    let mut bad_opts = base_opts(dir.path(), &image_dir, &raw_disk);
    bad_opts.owner_tag = Some("../evil tag".to_owned());
    assert!(
        create_local_vm_with_disk_stager(&store, &vmm, &bad_opts, |_, _, _, _| {
            panic!("must not run disk stager on invalid owner tag")
        })
        .is_err()
    );
    assert!(stub.state.lock().unwrap().calls.is_empty());

    // Drop CreateVm response once to verify owner_tag is persisted in Creating state AND
    // sent in CreateVm annotations AND verified on adoption.
    stub.state.lock().unwrap().drop_create_response_once = true;
    let mut opts = base_opts(dir.path(), &image_dir, &raw_disk);
    opts.owner_tag = Some(tag.clone());
    assert!(
        create_local_vm_with_disk_stager(&store, &vmm, &opts, |home, id, _, _| {
            Ok(home.join("vm").join(id).join("hda.img"))
        })
        .is_err()
    );

    // Creating phase status must already expose owner_tag and resource fields.
    let leases = dir.path().join("dhcp-leases");
    let creating_status = status_local_vm(&store, &vmm, "dev", &leases).unwrap();
    assert_eq!(creating_status.phase, LocalVmPhase::Creating);
    assert_eq!(creating_status.owner_tag.as_deref(), Some(tag.as_str()));
    assert_eq!(creating_status.app_id, calc_app_id(APP_COMPOSE_JSON));
    assert!(!creating_status.no_tee);
    assert_eq!(creating_status.vcpu, 3);
    assert_eq!(creating_status.memory_gib, 5);
    assert_eq!(creating_status.disk_gib, 10);

    // VMM must have received OWNER_TAG_ANNOTATION on CreateVm.
    {
        let g = stub.state.lock().unwrap();
        let vm = g.vms.get(UUID_OWNED).unwrap();
        assert_eq!(
            vm.annotations.get(OWNER_TAG_ANNOTATION).map(String::as_str),
            Some(tag.as_str())
        );
    }

    // Retrying with a different owner_tag must be rejected by immutable param check.
    let mut wrong_tag_opts = opts.clone();
    wrong_tag_opts.owner_tag = Some("outbe-net:other:89abcdef:validator-0".to_owned());
    let err = create_local_vm_with_disk_stager(&store, &vmm, &wrong_tag_opts, |_, _, _, _| {
        panic!("must not resume with mismatched owner_tag")
    })
    .unwrap_err();
    assert!(err.to_string().contains("owner_tag"), "{err}");

    // Tampering the VMM VM's owner_tag annotation must cause adoption to refuse.
    stub.state
        .lock()
        .unwrap()
        .vms
        .get_mut(UUID_OWNED)
        .unwrap()
        .annotations
        .insert(
            OWNER_TAG_ANNOTATION.to_owned(),
            "outbe-net:tampered:00000000:validator-0".to_owned(),
        );
    let err = create_local_vm_with_disk_stager(&store, &vmm, &opts, |_, _, _, _| {
        panic!("must not adopt VM with mismatched owner_tag annotation")
    })
    .unwrap_err();
    assert!(err.to_string().contains("owner_tag"), "{err}");

    // Restore exact annotation -> adoption succeeds and Ready status reports all fields.
    stub.state
        .lock()
        .unwrap()
        .vms
        .get_mut(UUID_OWNED)
        .unwrap()
        .annotations
        .insert(OWNER_TAG_ANNOTATION.to_owned(), tag.clone());
    let created = create_local_vm_with_disk_stager(&store, &vmm, &opts, |home, id, _, _| {
        Ok(home.join("vm").join(id).join("hda.img"))
    })
    .unwrap();
    assert_eq!(created.owner_tag.as_deref(), Some(tag.as_str()));
    let ready_status = status_local_vm(&store, &vmm, "dev", &leases).unwrap();
    assert_eq!(ready_status.owner_tag.as_deref(), Some(tag.as_str()));
    assert_eq!(ready_status.app_id, calc_app_id(APP_COMPOSE_JSON));
    assert!(!ready_status.no_tee);
    assert_eq!(ready_status.vcpu, 3);
    assert_eq!(ready_status.memory_gib, 5);
    assert_eq!(ready_status.disk_gib, 10);
}

#[test]
fn c1_expect_vm_id_enforced_before_any_vmm_rpc() {
    let dir = tempfile::tempdir().unwrap();
    let (image_dir, raw_disk) = make_image_and_disk(dir.path());
    let store = LocalVmStore::new(dir.path().join("state"));
    let stub = StubVmm::new();
    let vmm = VmmClient::new(stub.clone(), "stub");
    let opts = base_opts(dir.path(), &image_dir, &raw_disk);

    let created = create_local_vm_with_disk_stager(&store, &vmm, &opts, |home, id, _, _| {
        Ok(home.join("vm").join(id).join("hda.img"))
    })
    .unwrap();
    assert_eq!(created.vm_id, UUID_OWNED);

    let leases = dir.path().join("dhcp-leases");
    let calls_before = stub.state.lock().unwrap().calls.len();

    // Wrong UUID fails closed on every lifecycle verb with ZERO VMM RPCs.
    let err = start_local_vm_with_expected_id(&store, &vmm, "dev", Some(UUID_FOREIGN)).unwrap_err();
    assert!(err.to_string().contains("vm_id_mismatch"), "{err}");

    let err = stop_local_vm_with_expected_id(
        &store,
        &vmm,
        "dev",
        Some(UUID_FOREIGN),
        StopLocalVmOptions {
            force: false,
            timeout: Duration::from_secs(2),
            poll_interval: Duration::from_millis(10),
        },
    )
    .unwrap_err();
    assert!(err.to_string().contains("vm_id_mismatch"), "{err}");

    let err = status_local_vm_with_expected_id(&store, &vmm, "dev", Some(UUID_FOREIGN), &leases)
        .unwrap_err();
    assert!(err.to_string().contains("vm_id_mismatch"), "{err}");

    let err =
        logs_local_vm_with_expected_id(&store, &vmm, "dev", Some(UUID_FOREIGN), 10).unwrap_err();
    assert!(err.to_string().contains("vm_id_mismatch"), "{err}");

    let mut rm_mismatch = fast_remove_opts();
    rm_mismatch.expect_vm_id = Some(UUID_FOREIGN.to_owned());
    let err = remove_local_vm(&store, &vmm, "dev", rm_mismatch).unwrap_err();
    assert!(err.to_string().contains("vm_id_mismatch"), "{err}");

    assert_eq!(
        stub.state.lock().unwrap().calls.len(),
        calls_before,
        "mismatched --expect-vm-id must issue ZERO VMM RPCs"
    );
    // State file must still be Ready (not mutated to Removing).
    let guard = store.lock_vm("dev").unwrap();
    assert_eq!(
        store.load_locked(&guard, "dev").unwrap().phase,
        LocalVmPhase::Ready
    );
    drop(guard);

    // Matching UUID succeeds across status -> start -> logs -> stop -> remove.
    let st =
        status_local_vm_with_expected_id(&store, &vmm, "dev", Some(UUID_OWNED), &leases).unwrap();
    assert_eq!(st.vm_id, UUID_OWNED);
    start_local_vm_with_expected_id(&store, &vmm, "dev", Some(UUID_OWNED)).unwrap();
    let logs = logs_local_vm_with_expected_id(&store, &vmm, "dev", Some(UUID_OWNED), 5).unwrap();
    assert!(logs.log.contains(UUID_OWNED));
    stop_local_vm_with_expected_id(
        &store,
        &vmm,
        "dev",
        Some(UUID_OWNED),
        StopLocalVmOptions {
            force: false,
            timeout: Duration::from_secs(2),
            poll_interval: Duration::from_millis(10),
        },
    )
    .unwrap();
    let mut rm_match = fast_remove_opts();
    rm_match.expect_vm_id = Some(UUID_OWNED.to_owned());
    let removed = remove_local_vm(&store, &vmm, "dev", rm_match).unwrap();
    assert_eq!(removed.vm_id, UUID_OWNED);
}

#[test]
fn c1_staging_store_list_clean_symlink_refusal_and_consume_only_on_ready() {
    let dir = tempfile::tempdir().unwrap();
    let (image_dir, _) = make_image_and_disk(dir.path());
    let store = LocalVmStore::new(dir.path().join("state"));
    let stub = StubVmm::new();
    let vmm = VmmClient::new(stub.clone(), "stub");

    // Empty store returns empty list and idempotent clean.
    assert!(store.list_staged_disks().unwrap().is_empty());
    let noop = store.clean_staging_dir("dev").unwrap();
    assert!(!noop.removed);

    // Populate a valid staged disk (0700 dir, 0600 data.img).
    let staging_dir = store.ensure_staging_dir("dev").unwrap();
    let staged_img = store.staged_disk_path("dev").unwrap();
    fs::write(&staged_img, b"staged-raw-disk-bytes").unwrap();
    fs::set_permissions(&staged_img, fs::Permissions::from_mode(0o600)).unwrap();

    let listed = store.list_staged_disks().unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].staging_name, "dev");
    assert_eq!(listed[0].path.as_deref(), Some(staged_img.as_path()));
    assert_eq!(
        listed[0].size_bytes,
        Some(b"staged-raw-disk-bytes".len() as u64)
    );
    assert_eq!(
        listed[0].sha256.as_deref(),
        Some(sha256_sparse_file(&staged_img).unwrap().as_str())
    );
    assert!(listed[0].error.is_none());

    // Symlink staging dir is refused by clean_staging_dir, while list_staged_disks reports it
    // as an errored entry without aborting valid siblings (F5).
    let outside = dir.path().join("outside");
    fs::create_dir_all(&outside).unwrap();
    let evil_link = store.staging_root().join("evil");
    std::os::unix::fs::symlink(&outside, &evil_link).unwrap();
    assert!(store.clean_staging_dir("evil").is_err());
    let listed_with_bad = store.list_staged_disks().unwrap();
    assert_eq!(listed_with_bad.len(), 2);
    let dev_entry = listed_with_bad
        .iter()
        .find(|e| e.staging_name == "dev")
        .unwrap();
    assert!(dev_entry.error.is_none());
    let evil_entry = listed_with_bad
        .iter()
        .find(|e| e.staging_name == "evil")
        .unwrap();
    assert!(evil_entry.error.is_some());
    assert!(evil_entry.sha256.is_none());
    fs::remove_file(&evil_link).unwrap();

    // Failed create with disk_staging + consume_staging MUST preserve staged disk for retry!
    let mut opts = base_opts(dir.path(), &image_dir, &staged_img);
    opts.disk_staging_name = Some("dev".to_owned());
    opts.consume_staging = true;
    let err = create_local_vm_with_disk_stager(&store, &vmm, &opts, |_, _, _, _| {
        Err(eyre::eyre!("simulated qemu-img failure"))
    })
    .unwrap_err();
    assert!(
        err.to_string().contains("simulated qemu-img failure"),
        "{err}"
    );
    assert!(
        staged_img.is_file(),
        "staged disk must be preserved when vm create fails before Ready"
    );
    assert_eq!(store.list_staged_disks().unwrap().len(), 1);

    // Successful resume reaches Ready and consumes the staging directory!
    let created = create_local_vm_with_disk_stager(&store, &vmm, &opts, |home, id, _, _| {
        Ok(home.join("vm").join(id).join("hda.img"))
    })
    .unwrap();
    assert_eq!(created.phase, LocalVmPhase::Ready);
    assert!(created.staging_consumed);
    assert!(created.warning.is_none());
    assert!(!staging_dir.exists());
    assert!(store.list_staged_disks().unwrap().is_empty());
}

#[test]
fn c1_f2_vm_create_disk_staging_holds_staging_lock_across_hash_and_convert() {
    use std::sync::mpsc;

    let dir = tempfile::tempdir().unwrap();
    let (image_dir, _) = make_image_and_disk(dir.path());
    let store = LocalVmStore::new(dir.path().join("state"));
    let stub = StubVmm::new();

    store.ensure_staging_dir("dev").unwrap();
    let staged_img = store.staged_disk_path("dev").unwrap();
    fs::write(&staged_img, b"original-staged-bytes").unwrap();
    fs::set_permissions(&staged_img, fs::Permissions::from_mode(0o600)).unwrap();
    let expected_sha = sha256_sparse_file(&staged_img).unwrap();

    let (stager_entered_tx, stager_entered_rx) = mpsc::channel();
    let (allow_stager_tx, allow_stager_rx) = mpsc::channel();

    let store_create = store.clone();
    let vmm_create = VmmClient::new(stub.clone(), "stub");
    let mut opts = base_opts(dir.path(), &image_dir, &staged_img);
    opts.disk_staging_name = Some("dev".to_owned());
    // Even WITHOUT `--consume-staging`, `--disk-staging` MUST hold `lock_staging` across
    // hashing and conversion so a concurrent prepare/clean cannot race or swap `data.img`.
    opts.consume_staging = false;

    let create_handle = thread::spawn(move || {
        create_local_vm_with_disk_stager(
            &store_create,
            &vmm_create,
            &opts,
            |home, id, raw_disk, _| {
                stager_entered_tx.send(()).unwrap();
                allow_stager_rx.recv().unwrap();
                // Verify bytes converted match the exact hashed bytes untouched by races.
                assert_eq!(fs::read(raw_disk).unwrap(), b"original-staged-bytes");
                Ok(home.join("vm").join(id).join("hda.img"))
            },
        )
    });

    stager_entered_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("vm create did not reach disk_stager");

    // Probe the staging lock directly: MUST fail with WOULDBLOCK while vm create is running!
    let lock_path = store.staging_root().join(".dev.lock");
    let probe_fd = openat(
        CWD,
        &lock_path,
        OFlags::RDWR | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .unwrap();
    assert_eq!(
        flock(&probe_fd, FlockOperation::NonBlockingLockExclusive),
        Err(rustix::io::Errno::WOULDBLOCK),
        "vm create --disk-staging must hold exclusive staging lock across hash and convert"
    );
    drop(probe_fd);

    // Concurrent `clean_staging_dir("dev")` MUST block until vm create releases the lock.
    let (clean_done_tx, clean_done_rx) = mpsc::channel();
    let store_clean = store.clone();
    let clean_handle = thread::spawn(move || {
        let res = store_clean.clean_staging_dir("dev");
        let _ = clean_done_tx.send(res);
    });
    assert!(
        matches!(
            clean_done_rx.recv_timeout(Duration::from_millis(150)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ),
        "clean_staging_dir must block while vm create --disk-staging holds staging lock"
    );

    allow_stager_tx.send(()).unwrap();
    let created = create_handle.join().unwrap().expect("vm create failed");
    assert_eq!(created.phase, LocalVmPhase::Ready);

    let cleaned = clean_done_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("clean_staging_dir did not finish after vm create released lock")
        .unwrap();
    clean_handle.join().unwrap();
    assert!(cleaned.removed);

    let g = store.lock_vm("dev").unwrap();
    let st = store.load_locked(&g, "dev").unwrap();
    assert_eq!(
        st.disk_source_sha256.as_deref(),
        Some(expected_sha.as_str())
    );
}

#[test]
fn c1_f3_consume_staging_failure_after_ready_returns_ok_with_warning_and_false() {
    let dir = tempfile::tempdir().unwrap();
    let (image_dir, _) = make_image_and_disk(dir.path());
    let store = LocalVmStore::new(dir.path().join("state"));
    let stub = StubVmm::new();
    let vmm = VmmClient::new(stub, "stub");

    let staging_dir = store.ensure_staging_dir("dev").unwrap();
    let staged_img = store.staged_disk_path("dev").unwrap();
    fs::write(&staged_img, b"staged-raw-disk-bytes").unwrap();
    fs::set_permissions(&staged_img, fs::Permissions::from_mode(0o600)).unwrap();

    let outside = dir.path().join("outside-target");
    fs::create_dir_all(&outside).unwrap();

    let mut opts = base_opts(dir.path(), &image_dir, &staged_img);
    opts.disk_staging_name = Some("dev".to_owned());
    opts.consume_staging = true;

    // Simulate a post-conversion filesystem/security failure during staging consumption by
    // replacing `staging/dev` with a symlink after conversion completes. `LocalVmPhase::Ready`
    // is saved durably; consume fails closed on the symlink, so `create` MUST still succeed
    // (exit 0 equivalent) with `staging_consumed: false` and a clear warning.
    let staging_dir_clone = staging_dir.clone();
    let outside_clone = outside.clone();
    let created = create_local_vm_with_disk_stager(&store, &vmm, &opts, |home, id, _, _| {
        fs::remove_dir_all(&staging_dir_clone).unwrap();
        std::os::unix::fs::symlink(&outside_clone, &staging_dir_clone).unwrap();
        Ok(home.join("vm").join(id).join("hda.img"))
    })
    .expect("consume failure after durable Ready must not fail vm create");

    assert_eq!(created.phase, LocalVmPhase::Ready);
    assert!(!created.staging_consumed);
    let warning = created
        .warning
        .as_deref()
        .expect("consume failure must populate warning");
    assert!(
        warning.contains("disk clean --staging-name dev"),
        "{warning}"
    );
    // Outside target was never touched and VM state is durably Ready.
    assert!(outside.is_dir());
    let g = store.lock_vm("dev").unwrap();
    assert_eq!(
        store.load_locked(&g, "dev").unwrap().phase,
        LocalVmPhase::Ready
    );
}

#[test]
fn c1_f5_disk_clean_list_reports_bad_entries_without_aborting_valid_siblings() {
    let dir = tempfile::tempdir().unwrap();
    let store = LocalVmStore::new(dir.path().join("state"));

    // Two valid staged disks.
    for name in ["validator-0", "validator-1"] {
        store.ensure_staging_dir(name).unwrap();
        let img = store.staged_disk_path(name).unwrap();
        fs::write(&img, format!("disk-bytes-{name}")).unwrap();
        fs::set_permissions(&img, fs::Permissions::from_mode(0o600)).unwrap();
    }
    // 1. Unsafe mode (0755) directory.
    let bad_mode = store.staging_root().join("bad-mode");
    fs::create_dir_all(&bad_mode).unwrap();
    fs::set_permissions(&bad_mode, fs::Permissions::from_mode(0o755)).unwrap();
    // 2. Symlink entry.
    let outside = dir.path().join("outside");
    fs::create_dir_all(&outside).unwrap();
    std::os::unix::fs::symlink(&outside, store.staging_root().join("evil-link")).unwrap();
    // 3. Regular file directly under staging/.
    fs::write(store.staging_root().join("stray-file"), b"junk").unwrap();
    // 4. Valid dir with unsafe-mode (0644) data.img.
    store.ensure_staging_dir("bad-img-mode").unwrap();
    let bad_img = store.staged_disk_path("bad-img-mode").unwrap();
    fs::write(&bad_img, b"loose").unwrap();
    fs::set_permissions(&bad_img, fs::Permissions::from_mode(0o644)).unwrap();

    let entries = store.list_staged_disks().unwrap();
    assert_eq!(entries.len(), 6);

    for ok_name in ["validator-0", "validator-1"] {
        let e = entries.iter().find(|e| e.staging_name == ok_name).unwrap();
        assert!(
            e.error.is_none(),
            "expected valid entry for {ok_name}: {e:?}"
        );
        assert!(e.sha256.as_deref().is_some_and(|s| s.len() == 64));
        assert!(e.size_bytes.is_some());
    }
    for bad_name in ["bad-mode", "evil-link", "stray-file", "bad-img-mode"] {
        let e = entries.iter().find(|e| e.staging_name == bad_name).unwrap();
        assert!(
            e.error.is_some(),
            "expected error reported for {bad_name}: {e:?}"
        );
        assert!(e.sha256.is_none());
        assert!(e.size_bytes.is_none());
    }
}

#[test]
fn s7_update_local_vm_graceful_idempotent_and_never_force_stops() {
    let dir = tempfile::tempdir().unwrap();
    let (image_v1, disk) = make_image_and_disk(dir.path());
    let image_v2 = dir.path().join("image-v2");
    fs::create_dir_all(&image_v2).unwrap();
    let v2_digest = "2".repeat(64);
    fs::write(
        image_v2.join("metadata.json"),
        "{\"rootfs\":\"rootfs.img\"}\n",
    )
    .unwrap();
    fs::write(image_v2.join("digest.txt"), format!("{v2_digest}\n")).unwrap();
    fs::write(image_v2.join("rootfs.img"), b"fake-rootfs-v2").unwrap();

    let store = LocalVmStore::new(dir.path().join("state"));
    let stub = StubVmm::new();
    let vmm = VmmClient::new(stub.clone(), "stub");
    let opts = base_opts(dir.path(), &image_v1, &disk);
    let vmm_home = opts.vmm_home.clone();

    let created = create_local_vm_with_disk_stager(&store, &vmm, &opts, |home, id, _, _| {
        let dst = home.join("vm").join(id).join("hda.img");
        fs::create_dir_all(dst.parent().unwrap()).unwrap();
        fs::write(&dst, b"precious-validator-hda").unwrap();
        Ok(dst)
    })
    .unwrap();
    start_local_vm(&store, &vmm, "dev").unwrap();

    let hda_path = vmm_home.join("vm").join(&created.vm_id).join("hda.img");
    let prev_image_name = created.image_name.clone();
    let prev_hash = created.os_image_hash.clone();

    // 1. Missing target image fails closed BEFORE stopping the running VM.
    let missing_err = update_local_vm(
        &store,
        &vmm,
        &UpdateLocalVmOptions {
            name: "dev".into(),
            expect_vm_id: Some(created.vm_id.clone()),
            image: PathBuf::from("outbe-vm-dev-missing"),
            image_name: None,
            flavor: "dev".into(),
            vmm_home: vmm_home.clone(),
            stop_timeout: Duration::from_secs(5),
            poll_interval: Duration::from_millis(5),
        },
    )
    .unwrap_err();
    assert!(
        format!("{missing_err:#}").contains("not found"),
        "{missing_err:#}"
    );
    assert!(stub.state.lock().unwrap().vms[&created.vm_id].running);

    // 2. Wrong --expect-vm-id fails closed without touching VMM.
    let bad_id_err = update_local_vm(
        &store,
        &vmm,
        &UpdateLocalVmOptions {
            name: "dev".into(),
            expect_vm_id: Some("00000000-0000-4000-8000-000000000099".into()),
            image: image_v2.clone(),
            image_name: None,
            flavor: "dev".into(),
            vmm_home: vmm_home.clone(),
            stop_timeout: Duration::from_secs(5),
            poll_interval: Duration::from_millis(5),
        },
    )
    .unwrap_err();
    assert!(
        format!("{bad_id_err:#}").contains("vm_id_mismatch"),
        "{bad_id_err:#}"
    );

    // 3. Graceful stop timeout MUST fail closed and NEVER call StopVm.
    stub.state.lock().unwrap().ignore_shutdown = true;
    stub.state.lock().unwrap().calls.clear();
    let timeout_err = update_local_vm(
        &store,
        &vmm,
        &UpdateLocalVmOptions {
            name: "dev".into(),
            expect_vm_id: Some(created.vm_id.clone()),
            image: image_v2.clone(),
            image_name: None,
            flavor: "dev".into(),
            vmm_home: vmm_home.clone(),
            stop_timeout: Duration::from_millis(40),
            poll_interval: Duration::from_millis(5),
        },
    )
    .unwrap_err();
    assert!(
        format!("{timeout_err:#}").contains("without force-stop"),
        "{timeout_err:#}"
    );
    {
        let calls = &stub.state.lock().unwrap().calls;
        assert!(calls.iter().any(|c| c == "ShutdownVm"));
        assert!(
            !calls.iter().any(|c| c == "StopVm"),
            "must NEVER call StopVm: {calls:?}"
        );
        assert!(!calls.iter().any(|c| c == "UpdateVm"));
    }

    // 4. Happy path: graceful ShutdownVm -> UpdateVm -> StartVm preserving vm_id, MAC, and hda.img.
    stub.state.lock().unwrap().ignore_shutdown = false;
    stub.state.lock().unwrap().calls.clear();
    let updated = update_local_vm(
        &store,
        &vmm,
        &UpdateLocalVmOptions {
            name: "dev".into(),
            expect_vm_id: Some(created.vm_id.clone()),
            image: image_v2.clone(),
            image_name: None,
            flavor: "dev".into(),
            vmm_home: vmm_home.clone(),
            stop_timeout: Duration::from_secs(5),
            poll_interval: Duration::from_millis(5),
        },
    )
    .unwrap();
    assert_eq!(updated.vm_id, created.vm_id);
    assert_eq!(updated.previous_image_name, prev_image_name);
    assert_eq!(updated.previous_os_image_hash, prev_hash);
    assert_eq!(
        updated.image_name,
        format!("outbe-vm-dev-{}", &v2_digest[..16])
    );
    assert_eq!(updated.os_image_hash, v2_digest);
    assert!(!updated.already_updated);
    assert_eq!(updated.vmm_status, VmInfo::STATUS_RUNNING);
    // hda.img untouched!
    assert_eq!(fs::read(&hda_path).unwrap(), b"precious-validator-hda");
    {
        let calls = &stub.state.lock().unwrap().calls;
        assert!(!calls.iter().any(|c| c == "StopVm"));
        let shutdown_idx = calls.iter().position(|c| c == "ShutdownVm").unwrap();
        let update_idx = calls.iter().position(|c| c == "UpdateVm").unwrap();
        let start_idx = calls.iter().position(|c| c == "StartVm").unwrap();
        assert!(
            shutdown_idx < update_idx && update_idx < start_idx,
            "{calls:?}"
        );
    }

    // 5. Idempotent re-run referencing published image name directly returns already_updated=true
    //    without stopping or restarting the VM.
    stub.state.lock().unwrap().calls.clear();
    let rerun = update_local_vm(
        &store,
        &vmm,
        &UpdateLocalVmOptions {
            name: "dev".into(),
            expect_vm_id: Some(created.vm_id.clone()),
            image: PathBuf::from(&updated.image_name),
            image_name: None,
            flavor: "dev".into(),
            vmm_home: vmm_home.clone(),
            stop_timeout: Duration::from_secs(5),
            poll_interval: Duration::from_millis(5),
        },
    )
    .unwrap();
    assert!(rerun.already_updated);
    assert_eq!(rerun.image_name, updated.image_name);
    assert_eq!(rerun.os_image_hash, v2_digest);
    {
        let calls = &stub.state.lock().unwrap().calls;
        assert!(
            !calls
                .iter()
                .any(|c| matches!(c.as_str(), "ShutdownVm" | "StopVm" | "UpdateVm" | "StartVm")),
            "{calls:?}"
        );
    }
}

#[test]
fn s7_update_local_vm_fails_closed_if_vmm_never_reports_running() {
    let dir = tempfile::tempdir().unwrap();
    let (image_v1, disk) = make_image_and_disk(dir.path());
    let image_v2 = dir.path().join("image-v2");
    fs::create_dir_all(&image_v2).unwrap();
    let v2_digest = "4".repeat(64);
    fs::write(
        image_v2.join("metadata.json"),
        "{\"rootfs\":\"rootfs.img\"}\n",
    )
    .unwrap();
    fs::write(image_v2.join("digest.txt"), format!("{v2_digest}\n")).unwrap();
    fs::write(image_v2.join("rootfs.img"), b"fake-rootfs-v2").unwrap();

    let store = LocalVmStore::new(dir.path().join("state"));
    let stub = StubVmm::new();
    let vmm = VmmClient::new(stub.clone(), "stub");
    let opts = base_opts(dir.path(), &image_v1, &disk);
    let vmm_home = opts.vmm_home.clone();

    let created = create_local_vm_with_disk_stager(&store, &vmm, &opts, |home, id, _, _| {
        let dst = home.join("vm").join(id).join("hda.img");
        fs::create_dir_all(dst.parent().unwrap()).unwrap();
        fs::write(&dst, b"precious-validator-hda").unwrap();
        Ok(dst)
    })
    .unwrap();
    start_local_vm(&store, &vmm, "dev").unwrap();

    // F6: StartVm succeeds at RPC layer, but VMM GetInfo never transitions to `running`.
    stub.state.lock().unwrap().ignore_start = true;
    let err = update_local_vm(
        &store,
        &vmm,
        &UpdateLocalVmOptions {
            name: "dev".into(),
            expect_vm_id: Some(created.vm_id.clone()),
            image: image_v2.clone(),
            image_name: None,
            flavor: "dev".into(),
            vmm_home: vmm_home.clone(),
            stop_timeout: Duration::from_millis(80),
            poll_interval: Duration::from_millis(5),
        },
    )
    .unwrap_err();
    assert!(
        format!("{err:#}").contains("did not reach `running` state after StartVm"),
        "{err:#}"
    );
    assert!(!stub.state.lock().unwrap().vms[&created.vm_id].running);

    // Resuming after StartVm works again skips ShutdownVm & UpdateVm and confirms `running`.
    stub.state.lock().unwrap().ignore_start = false;
    stub.state.lock().unwrap().calls.clear();
    let resumed = update_local_vm(
        &store,
        &vmm,
        &UpdateLocalVmOptions {
            name: "dev".into(),
            expect_vm_id: Some(created.vm_id.clone()),
            image: image_v2,
            image_name: None,
            flavor: "dev".into(),
            vmm_home,
            stop_timeout: Duration::from_secs(5),
            poll_interval: Duration::from_millis(5),
        },
    )
    .unwrap();
    assert_eq!(resumed.vmm_status, VmInfo::STATUS_RUNNING);
    assert_eq!(resumed.os_image_hash, v2_digest);
    let calls = &stub.state.lock().unwrap().calls;
    assert!(!calls.iter().any(|c| c == "ShutdownVm"), "{calls:?}");
    assert!(!calls.iter().any(|c| c == "UpdateVm"), "{calls:?}");
    assert!(calls.iter().any(|c| c == "StartVm"), "{calls:?}");
}
