use std::{
    fs,
    io::{Read, Write},
    net::Ipv4Addr,
    os::unix::{
        fs::{symlink, PermissionsExt},
        net::UnixListener,
    },
    path::{Path, PathBuf},
    process::Command,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    thread,
    time::Duration,
};

use outbe_operator_vm::local::{LocalVmPhase, LocalVmState, LocalVmStore, LOCAL_VM_STATE_VERSION};
use serde_json::Value;

const OUTBE_UP_BIN: &str = env!("CARGO_BIN_EXE_outbe-up");
const TEST_VM_UUID: &str = "550e8400-e29b-41d4-a716-446655440099";

const AMD_DHCP_HOSTS: &str = "\
be:6a:63:f8:84:d0,10.77.0.11,validator-0,infinite
5a:9a:a8:0f:91:46,10.77.0.12,validator-1,infinite
e2:84:43:6d:da:2f,10.77.0.35,outbe-v2,infinite
a2:35:bc:88:16:86,10.77.0.38,outbe-v3,infinite

# outbe-up:d3-amd-73d044094:begin
ca:ae:de:a3:b1:11,10.77.0.60
02:72:11:32:b7:48,10.77.0.61
96:e6:d0:77:1f:3d,10.77.0.62
ea:14:6a:ee:cf:8b,10.77.0.63
# outbe-up:d3-amd-73d044094:end

# outbe-up:final-amd-f36afc309:begin
be:f7:7b:2c:d1:85,10.77.0.68,10m
7e:37:6b:2a:04:15,10.77.0.69,10m
4e:b6:05:72:58:5e,10.77.0.70,10m
06:62:f6:c6:97:20,10.77.0.71,10m
# outbe-up:final-amd-f36afc309:end
";

const AMD_DHCP_LEASES: &str = "\
1795598734 be:f7:7b:2c:d1:85 10.77.0.68 dstack *
1795598682 7e:37:6b:2a:04:15 10.77.0.69 * *
1795598615 4e:b6:05:72:58:5e 10.77.0.70 * *
1795598676 06:62:f6:c6:97:20 10.77.0.71 * *
0 be:6a:63:f8:84:d0 10.77.0.11 validator-0 *
0 5a:9a:a8:0f:91:46 10.77.0.12 validator-1 *
";

const AMD_NFT_JSON: &str = r#"{"nftables":[{"metainfo":{"version":"1.1.6","json_schema_version":1}},{"table":{"family":"inet","name":"host_firewall","handle":66}},{"chain":{"family":"inet","table":"host_firewall","name":"input","handle":1,"type":"filter","hook":"input","prio":0,"policy":"drop"}},{"chain":{"family":"inet","table":"host_firewall","name":"forward","handle":2,"type":"filter","hook":"forward","prio":0,"policy":"drop"}},{"chain":{"family":"inet","table":"host_firewall","name":"output","handle":3,"type":"filter","hook":"output","prio":0,"policy":"accept"}},{"rule":{"family":"inet","table":"host_firewall","chain":"forward","handle":698,"expr":[{"match":{"op":"in","left":{"ct":{"key":"state"}},"right":["established","related"]}},{"accept":null}]}},{"rule":{"family":"inet","table":"host_firewall","chain":"forward","handle":699,"expr":[{"match":{"op":"in","left":{"ct":{"key":"state"}},"right":"invalid"}},{"drop":null}]}},{"rule":{"family":"inet","table":"host_firewall","chain":"forward","handle":700,"comment":"outbound:validator_vms","expr":[{"match":{"op":"==","left":{"payload":{"protocol":"ip","field":"saddr"}},"right":{"prefix":{"addr":"10.77.0.0","len":24}}}},{"accept":null}]}},{"rule":{"family":"inet","table":"host_firewall","chain":"forward","handle":711,"comment":"validator-0-hyperlane","expr":[{"match":{"op":"==","left":{"payload":{"protocol":"ip","field":"saddr"}},"right":"141.95.147.184"}},{"match":{"op":"==","left":{"payload":{"protocol":"ip","field":"daddr"}},"right":"10.77.0.11"}},{"match":{"op":"in","left":{"ct":{"key":"status"}},"right":"dnat"}},{"match":{"op":"==","left":{"payload":{"protocol":"tcp","field":"dport"}},"right":9000}},{"accept":null}]}},{"rule":{"family":"inet","table":"host_firewall","chain":"forward","handle":721,"comment":"validator-1-hyperlane","expr":[{"match":{"op":"==","left":{"payload":{"protocol":"ip","field":"saddr"}},"right":"141.95.147.184"}},{"match":{"op":"==","left":{"payload":{"protocol":"ip","field":"daddr"}},"right":"10.77.0.12"}},{"match":{"op":"in","left":{"ct":{"key":"status"}},"right":"dnat"}},{"match":{"op":"==","left":{"payload":{"protocol":"tcp","field":"dport"}},"right":9000}},{"accept":null}]}},{"table":{"family":"ip","name":"host_firewall_nat","handle":67}},{"chain":{"family":"ip","table":"host_firewall_nat","name":"prerouting","handle":1,"type":"nat","hook":"prerouting","prio":-100,"policy":"accept"}},{"chain":{"family":"ip","table":"host_firewall_nat","name":"postrouting","handle":2,"type":"nat","hook":"postrouting","prio":100,"policy":"accept"}},{"rule":{"family":"ip","table":"host_firewall_nat","chain":"prerouting","handle":456,"comment":"validator-0-hyperlane","expr":[{"match":{"op":"==","left":{"payload":{"protocol":"ip","field":"daddr"}},"right":"162.19.107.121"}},{"match":{"op":"==","left":{"payload":{"protocol":"ip","field":"saddr"}},"right":"141.95.147.184"}},{"match":{"op":"==","left":{"payload":{"protocol":"tcp","field":"dport"}},"right":9000}},{"dnat":{"addr":"10.77.0.11"}}]}},{"rule":{"family":"ip","table":"host_firewall_nat","chain":"prerouting","handle":466,"comment":"validator-1-hyperlane","expr":[{"match":{"op":"==","left":{"payload":{"protocol":"ip","field":"daddr"}},"right":"162.19.107.121"}},{"match":{"op":"==","left":{"payload":{"protocol":"ip","field":"saddr"}},"right":"141.95.147.184"}},{"match":{"op":"==","left":{"payload":{"protocol":"tcp","field":"dport"}},"right":9001}},{"dnat":{"addr":"10.77.0.12","port":9000}}]}},{"rule":{"family":"ip","table":"host_firewall_nat","chain":"postrouting","handle":467,"comment":"hairpin:validator_vms","expr":[{"match":{"op":"==","left":{"payload":{"protocol":"ip","field":"saddr"}},"right":{"prefix":{"addr":"10.77.0.0","len":24}}}},{"match":{"op":"==","left":{"payload":{"protocol":"ip","field":"daddr"}},"right":{"prefix":{"addr":"10.77.0.0","len":24}}}},{"match":{"op":"in","left":{"ct":{"key":"status"}},"right":"dnat"}},{"snat":{"addr":"162.19.107.121"}}]}},{"rule":{"family":"ip","table":"host_firewall_nat","chain":"postrouting","handle":468,"comment":"outbound:validator_vms","expr":[{"match":{"op":"==","left":{"payload":{"protocol":"ip","field":"saddr"}},"right":{"prefix":{"addr":"10.77.0.0","len":24}}}},{"match":{"op":"!=","left":{"payload":{"protocol":"ip","field":"daddr"}},"right":{"prefix":{"addr":"10.77.0.0","len":24}}}},{"masquerade":null}]}}]}"#;

const INTEL_NFT_JSON: &str = r#"{"nftables":[{"metainfo":{"version":"1.1.6","json_schema_version":1}},{"table":{"family":"inet","name":"host_firewall","handle":66}},{"chain":{"family":"inet","table":"host_firewall","name":"forward","handle":2,"type":"filter","hook":"forward","prio":0,"policy":"drop"}},{"rule":{"family":"inet","table":"host_firewall","chain":"forward","handle":700,"comment":"outbound:validator_vms","expr":[{"match":{"op":"==","left":{"payload":{"protocol":"ip","field":"saddr"}},"right":{"prefix":{"addr":"10.77.0.0","len":24}}}},{"accept":null}]}},{"table":{"family":"ip","name":"host_firewall_nat","handle":67}},{"chain":{"family":"ip","table":"host_firewall_nat","name":"prerouting","handle":1,"type":"nat","hook":"prerouting","prio":-100,"policy":"accept"}},{"chain":{"family":"ip","table":"host_firewall_nat","name":"postrouting","handle":2,"type":"nat","hook":"postrouting","prio":100,"policy":"accept"}},{"rule":{"family":"ip","table":"host_firewall_nat","chain":"prerouting","handle":501,"comment":"validator-2-hyperlane","expr":[{"match":{"op":"==","left":{"payload":{"protocol":"ip","field":"daddr"}},"right":"141.95.168.110"}},{"match":{"op":"==","left":{"payload":{"protocol":"ip","field":"saddr"}},"right":"141.95.147.184"}},{"match":{"op":"==","left":{"payload":{"protocol":"tcp","field":"dport"}},"right":9000}},{"dnat":{"addr":"10.77.0.11"}}]}},{"rule":{"family":"ip","table":"host_firewall_nat","chain":"prerouting","handle":502,"comment":"validator-joiner-0-hyperlane","expr":[{"match":{"op":"==","left":{"payload":{"protocol":"ip","field":"daddr"}},"right":"141.95.168.110"}},{"match":{"op":"==","left":{"payload":{"protocol":"ip","field":"saddr"}},"right":"141.95.147.184"}},{"match":{"op":"==","left":{"payload":{"protocol":"tcp","field":"dport"}},"right":9002}},{"dnat":{"addr":"10.77.0.13","port":9000}}]}},{"rule":{"family":"ip","table":"host_firewall_nat","chain":"postrouting","handle":510,"comment":"outbound:validator_vms","expr":[{"match":{"op":"==","left":{"payload":{"protocol":"ip","field":"saddr"}},"right":{"prefix":{"addr":"10.77.0.0","len":24}}}},{"match":{"op":"!=","left":{"payload":{"protocol":"ip","field":"daddr"}},"right":{"prefix":{"addr":"10.77.0.0","len":24}}}},{"masquerade":null}]}}]}"#;

const EMPTY_NFT_JSON: &str =
    r#"{"nftables":[{"metainfo":{"version":"1.1.6","json_schema_version":1}}]}"#;

fn write_exec(path: &Path, content: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, content).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

fn write_file(path: &Path, content: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, content).unwrap();
}

/// Minimal Unix-socket fake VMM responding to `Version`, `GetMeta`, `Status`, and `GetInfo`.
struct FakeVmm {
    socket_path: PathBuf,
    stop: Arc<AtomicBool>,
    handle: Option<thread::JoinHandle<()>>,
}

impl FakeVmm {
    fn start(socket_path: PathBuf, vm_name: &str, mac: &str) -> Self {
        if let Some(parent) = socket_path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        let listener = UnixListener::bind(&socket_path).unwrap();
        listener.set_nonblocking(true).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let stop_bg = Arc::clone(&stop);
        let mac_owned = mac.to_owned();
        let vm_name_owned = vm_name.to_owned();

        let handle = thread::spawn(move || {
            while !stop_bg.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let _ = stream.set_nonblocking(false);
                        let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
                        let mut req = Vec::new();
                        let mut chunk = [0_u8; 4096];
                        let mut header_end = None;
                        let mut content_len = 0usize;
                        while let Ok(n) = stream.read(&mut chunk) {
                            if n == 0 {
                                break;
                            }
                            req.extend_from_slice(&chunk[..n]);
                            if header_end.is_none() {
                                if let Some(pos) = req.windows(4).position(|w| w == b"\r\n\r\n") {
                                    let end = pos + 4;
                                    header_end = Some(end);
                                    let head = String::from_utf8_lossy(&req[..end]);
                                    for line in head.lines() {
                                        if let Some((k, v)) = line.split_once(':') {
                                            if k.trim().eq_ignore_ascii_case("content-length") {
                                                content_len = v.trim().parse().unwrap_or(0);
                                            }
                                        }
                                    }
                                }
                            }
                            if let Some(end) = header_end {
                                if req.len() >= end + content_len {
                                    break;
                                }
                            }
                        }
                        let req_line = String::from_utf8_lossy(&req)
                            .lines()
                            .next()
                            .unwrap_or("")
                            .to_owned();
                        let mac_now = mac_owned.clone();
                        let body = if req_line.contains("/prpc/Version") {
                            r#"{"version":"0.6.3","rev":"git:c88f60e781a62d542e08"}"#.to_owned()
                        } else if req_line.contains("/prpc/GetMeta") {
                            r#"{"resources":{"max_cvm_number":1000,"max_allocable_vcpu":40,"max_allocable_memory_in_mb":100000},"networking":{"supported_modes":["user","bridge"],"default_mode":"user","default_bridge":"obr0","max_queues":16,"default_vhost":false}}"#.to_owned()
                        } else if req_line.contains("/prpc/GetInfo") {
                            serde_json::json!({
                                "found": true,
                                "info": {
                                    "id": TEST_VM_UUID,
                                    "name": vm_name_owned,
                                    "status": "stopped",
                                    "running": false,
                                    "interfaces": [{"mode": "bridge", "mac": mac_now}],
                                    "configuration": {"image": "outbe-vm-dev-3485ca2592b7a0c3"}
                                }
                            })
                            .to_string()
                        } else {
                            serde_json::json!({
                                "vms": [
                                    {"id": "cdbc5a01-481f-47f1-8474-cdac63ed9f32", "name": "v0", "status": "running", "running": true},
                                    {"id": "5bfcc4bb-653f-4d21-b63c-7485c975b71c", "name": "v1", "status": "running", "running": true},
                                    {"id": "e240d848-b9aa-4be8-8992-e4a400d4f5e3", "name": "v2", "status": "running", "running": true},
                                    {"id": "bb9a3a36-8ebe-4760-a8ca-585bb21c4c29", "name": "v3", "status": "running", "running": true}
                                ],
                                "total": 4
                            })
                            .to_string()
                        };
                        let resp = format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                            body.len()
                        );
                        let _ = stream.write_all(resp.as_bytes());
                        let _ = stream.flush();
                    }
                    Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(10));
                    }
                    Err(_) => break,
                }
            }
        });

        Self {
            socket_path,
            stop,
            handle: Some(handle),
        }
    }
}

impl Drop for FakeVmm {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
        let _ = fs::remove_file(&self.socket_path);
    }
}

struct HostFixture {
    _tmp: tempfile::TempDir,
    sys_root: PathBuf,
    bin_dir: PathBuf,
    vmm_home: PathBuf,
    vmm_socket: PathBuf,
    state_dir: PathBuf,
    hosts_file: PathBuf,
    leases_file: PathBuf,
    mutations_log: PathBuf,
    _vmm: FakeVmm,
}

impl HostFixture {
    fn build(profile: &str) -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let sys_root = tmp.path().join("sysroot");
        let bin_dir = tmp.path().join("bin");
        let vmm_home = tmp.path().join("vmm");
        let vmm_socket = vmm_home.join("run/vmm.sock");
        let state_dir = tmp.path().join("state");
        let hosts_file = sys_root.join("var/lib/validator-vm/dhcp-hosts");
        let leases_file = sys_root.join("var/lib/validator-vm/dhcp-leases");
        let mutations_log = tmp.path().join("mutations.log");
        let nft_fixture_file = tmp.path().join("nft.json");

        fs::create_dir_all(&sys_root).unwrap();
        fs::create_dir_all(&bin_dir).unwrap();

        let (uplink, pub_ip, nft_json, ip_fwd, bridge_present, dhcp_active, is_intel) =
            match profile {
                "amd" => (
                    "eno1",
                    "162.19.107.121",
                    AMD_NFT_JSON,
                    "1\n",
                    true,
                    true,
                    false,
                ),
                "intel" => (
                    "eno1np0",
                    "141.95.168.110",
                    INTEL_NFT_JSON,
                    "1\n",
                    true,
                    true,
                    true,
                ),
                "unconfigured" => (
                    "eno1",
                    "162.19.107.121",
                    EMPTY_NFT_JSON,
                    "0\n",
                    false,
                    false,
                    false,
                ),
                "clean_nat" => (
                    "eno1",
                    "162.19.107.121",
                    EMPTY_NFT_JSON,
                    "0\n",
                    true,
                    true,
                    false,
                ),
                other => panic!("unknown fixture profile {other}"),
            };

        write_file(&nft_fixture_file, nft_json);
        write_file(&sys_root.join("proc/sys/net/ipv4/ip_forward"), ip_fwd);
        if ip_fwd.trim() == "1" {
            write_file(
                &sys_root.join("etc/sysctl.d/90-vm-host.conf"),
                "net.ipv4.ip_forward=1\n",
            );
        }
        write_file(&sys_root.join("dev/kvm"), "");
        write_file(&sys_root.join("dev/vhost-vsock"), "");
        if is_intel {
            write_file(&sys_root.join("sys/module/kvm_intel/parameters/tdx"), "Y\n");
        } else if profile == "amd" {
            write_file(
                &sys_root.join("sys/module/kvm_amd/parameters/sev_snp"),
                "Y\n",
            );
        }
        write_file(&hosts_file, AMD_DHCP_HOSTS);
        write_file(&leases_file, AMD_DHCP_LEASES);

        // Stub `sudo`: strips `-n` and execs directly as current test user.
        write_exec(
            &bin_dir.join("sudo"),
            "#!/bin/sh\nif [ \"$1\" = \"-n\" ]; then shift; fi\nexec \"$@\"\n",
        );

        // Stub `sysctl`: reads from `<sys_root>/proc/sys/net/ipv4/ip_forward` and logs writes.
        write_exec(
            &bin_dir.join("sysctl"),
            &format!(
                r#"#!/bin/sh
if [ "$1" = "-n" ] && [ "$2" = "net.ipv4.ip_forward" ]; then
  cat '{}'
  exit 0
fi
echo "sysctl $*" >> '{}'
exit 0
"#,
                sys_root.join("proc/sys/net/ipv4/ip_forward").display(),
                mutations_log.display()
            ),
        );

        // Stub `ip`: serves bridge addr JSON, one-line bridge addr, uplink addr, and default route.
        let bridge_json = if bridge_present {
            r#"[{"ifindex":5,"ifname":"obr0","flags":["BROADCAST","MULTICAST","UP","LOWER_UP"],"operstate":"UP","addr_info":[{"family":"inet","local":"10.77.0.1","prefixlen":24}]}]"#
        } else {
            ""
        };
        let bridge_oneline = if bridge_present {
            "5: obr0    inet 10.77.0.1/24 brd 10.77.0.255 scope global obr0"
        } else {
            ""
        };
        write_exec(
            &bin_dir.join("ip"),
            &format!(
                r#"#!/bin/sh
args="$*"
case "$args" in
  "-j addr show dev obr0")
    if [ -n '{bridge_json}' ]; then
      printf '%s\n' '{bridge_json}'
      exit 0
    fi
    exit 1
    ;;
  "-4 -o addr show dev obr0")
    if [ -n '{bridge_oneline}' ]; then
      printf '%s\n' '{bridge_oneline}'
      exit 0
    fi
    exit 1
    ;;
  "-j -4 addr show dev {uplink}")
    printf '[{{"ifname":"{uplink}","operstate":"UP","flags":["UP"],"addr_info":[{{"family":"inet","local":"{pub_ip}","prefixlen":24}}]}}]\n'
    exit 0
    ;;
  "-4 -o route show default")
    printf 'default via 162.19.107.254 dev {uplink} proto static\n'
    exit 0
    ;;
  "link show dev {uplink}")
    exit 0
    ;;
  *)
    exit 1
    ;;
esac
"#
            ),
        );

        // Stub `networkctl`: reports unmanaged obr0 (real host behavior).
        write_exec(
            &bin_dir.join("networkctl"),
            "#!/bin/sh\necho 'Network File: n/a'\necho 'State: routable (unmanaged)'\nexit 0\n",
        );

        // Stub `nft`: serves `-j list ruleset` and logs any mutating calls.
        write_exec(
            &bin_dir.join("nft"),
            &format!(
                r#"#!/bin/sh
if [ "$1" = "-j" ] && [ "$2" = "list" ] && [ "$3" = "ruleset" ]; then
  cat '{}'
  exit 0
fi
echo "nft $*" >> '{}'
exit 0
"#,
                nft_fixture_file.display(),
                mutations_log.display()
            ),
        );

        // Stub `systemctl`: answers `show` accurately for each service and logs `reload`/`enable`/`disable`.
        let bridge_state = if bridge_present { "active" } else { "inactive" };
        let bridge_load = if bridge_present {
            "loaded"
        } else {
            "not-found"
        };
        let dhcp_state = if dhcp_active { "active" } else { "inactive" };
        let dhcp_load = if dhcp_active { "loaded" } else { "not-found" };
        let qgs_state = if is_intel { "active" } else { "inactive" };
        let qgs_enabled = if is_intel { "enabled" } else { "disabled" };
        write_exec(
            &bin_dir.join("systemctl"),
            &format!(
                r#"#!/bin/sh
if [ "$1" = "show" ]; then
  unit=""
  for arg in "$@"; do unit="$arg"; done
  case "$unit" in
    vm-bridge.service)
      printf 'LoadState={bridge_load}\nActiveState={bridge_state}\nUnitFileState=enabled\nMainPID=0\nExecStart={{ path=/usr/local/libexec/vm-bridge ; argv[]=/usr/local/libexec/vm-bridge up obr0 10.77.0.1/24 ; ignore_errors=no }}\nExecReload=\n'
      ;;
    validator-vm-dhcp.service)
      printf 'LoadState={dhcp_load}\nActiveState={dhcp_state}\nUnitFileState=enabled\nMainPID=219283\nExecStart={{ path=/usr/sbin/dnsmasq ; argv[]=/usr/sbin/dnsmasq --user=root --conf-file=/dev/null --interface=obr0 --bind-interfaces --except-interface=lo --port=0 --dhcp-ignore-clid --dhcp-range=10.77.0.100,10.77.0.200,255.255.255.0,5m --dhcp-hostsfile={} --dhcp-leasefile={} --pid-file=/run/validator-vm-dhcp.pid ; ignore_errors=no }}\nExecReload={{ path=/bin/kill ; argv[]=/bin/kill -HUP $MAINPID ; ignore_errors=no }}\n'
      ;;
    dstack-vmm.service)
      printf 'LoadState=loaded\nActiveState=inactive\nUnitFileState=enabled\nMainPID=0\nExecStart=/usr/local/bin/dstack-vmm -c ../vmm.toml\nExecReload=\n'
      ;;
    qgsd.service|pccs.service)
      printf 'LoadState=loaded\nActiveState={qgs_state}\nUnitFileState={qgs_enabled}\nMainPID=0\nExecStart=\nExecReload=\n'
      ;;
    *)
      printf 'LoadState=not-found\nActiveState=inactive\nUnitFileState=\nMainPID=0\nExecStart=\nExecReload=\n'
      ;;
  esac
  exit 0
fi
echo "systemctl $*" >> '{}'
exit 0
"#,
                hosts_file.display(),
                leases_file.display(),
                mutations_log.display()
            ),
        );

        let vmm = FakeVmm::start(vmm_socket.clone(), "node-a", "52:54:00:aa:bb:88");

        Self {
            _tmp: tmp,
            sys_root,
            bin_dir,
            vmm_home,
            vmm_socket,
            state_dir,
            hosts_file,
            leases_file,
            mutations_log,
            _vmm: vmm,
        }
    }

    fn path_env(&self) -> String {
        format!(
            "{}:{}",
            self.bin_dir.display(),
            std::env::var("PATH").unwrap_or_default()
        )
    }

    fn seed_vm(&self, name: &str, mac: &str, intended_ip: Option<Ipv4Addr>) {
        let store = LocalVmStore::new(&self.state_dir);
        let guard = store.lock_vm(name).unwrap();
        let state = LocalVmState {
            version: LOCAL_VM_STATE_VERSION,
            name: name.to_owned(),
            vm_id: TEST_VM_UUID.to_owned(),
            create_token: None,
            vmm_socket: self.vmm_socket.clone(),
            vmm_home: self.vmm_home.clone(),
            image_name: "outbe-vm-dev-3485ca2592b7a0c3".to_owned(),
            os_image_hash: "3485ca2592b7a0c3".repeat(4),
            disk_source: PathBuf::from("/tmp/disk.raw"),
            disk_source_sha256: Some("0".repeat(64)),
            vcpu: 3,
            memory_gib: 5,
            disk_gib: 40,
            bridge: "obr0".to_owned(),
            intended_ip,
            mac: mac.to_owned(),
            app_id: "a".repeat(40),
            no_tee: false,
            phase: LocalVmPhase::Ready,
            journal: Vec::new(),
        };
        store.save_locked(&guard, &state).unwrap();
    }

    fn mutations(&self) -> String {
        fs::read_to_string(&self.mutations_log).unwrap_or_default()
    }
}

#[test]
fn host_status_amd_and_intel_fixtures_emit_v1_schema_drift_and_qgs() {
    let amd = HostFixture::build("amd");
    let out = Command::new(OUTBE_UP_BIN)
        .env("PATH", amd.path_env())
        .args(["host", "status", "--vmm-socket"])
        .arg(&amd.vmm_socket)
        .arg("--vmm-home")
        .arg(&amd.vmm_home)
        .arg("--sys-root")
        .arg(&amd.sys_root)
        .arg("--json")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "host status amd failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let json: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(json["version"], 1);
    assert_eq!(json["bridge"]["name"], "obr0");
    assert_eq!(json["bridge"]["cidr"], "10.77.0.0/24");
    assert_eq!(json["bridge"]["gateway_ip"], "10.77.0.1");
    assert_eq!(json["bridge"]["management"], "systemd:vm-bridge.service");
    assert_eq!(json["dhcp"]["unit_name"], "validator-vm-dhcp.service");
    assert_eq!(json["dhcp"]["pool_start"], "10.77.0.100");
    assert_eq!(json["dhcp"]["pool_end"], "10.77.0.200");
    assert_eq!(json["dhcp"]["lease_duration"], "5m");
    assert_eq!(json["dhcp"]["reload_mode"], "systemctl-reload");
    assert_eq!(json["dhcp"]["reservations_count"], 12);
    assert_eq!(json["dhcp"]["active_leases_count"], 6);
    assert_eq!(json["firewall"]["nft_accessible"], true);
    assert_eq!(json["firewall"]["forward_chains"][0]["policy"], "drop");
    assert_eq!(json["firewall"]["dnat_rules"].as_array().unwrap().len(), 2);
    assert_eq!(json["nat"]["uplink"], "eno1");
    assert_eq!(json["nat"]["egress_status"], "foreign_provided");
    assert_eq!(json["nat"]["ip_forward_runtime"], 1);
    assert_eq!(json["nat"]["ip_forward_persistent"]["effective_value"], 1);
    assert_eq!(json["vmm"]["reachable"], true);
    assert_eq!(json["vmm"]["version"], "0.6.3");
    assert_eq!(json["vmm"]["default_mode"], "user");
    assert_eq!(json["vmm"]["running_vms"], 4);
    assert!(
        json["vmm"]["drift_notes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v.as_str().unwrap_or("").contains("dstack-vmm.service")),
        "missing VMM unit drift note: {}",
        json["vmm"]["drift_notes"]
    );
    assert_eq!(json["platform"]["tee_kind"], "amd-sev-snp");
    assert_eq!(json["platform"]["sev_snp_enabled"], true);
    assert_eq!(json["platform"]["qgs"]["required"], false);
    assert_eq!(json["ready_for_vm"], true);
    assert!(
        amd.mutations().is_empty(),
        "host status must perform zero writes"
    );

    let intel = HostFixture::build("intel");
    let out_i = Command::new(OUTBE_UP_BIN)
        .env("PATH", intel.path_env())
        .args(["host", "status", "--vmm-socket"])
        .arg(&intel.vmm_socket)
        .arg("--vmm-home")
        .arg(&intel.vmm_home)
        .arg("--sys-root")
        .arg(&intel.sys_root)
        .arg("--json")
        .output()
        .unwrap();
    assert!(out_i.status.success());
    let ji: Value = serde_json::from_slice(&out_i.stdout).unwrap();
    assert_eq!(ji["platform"]["tee_kind"], "intel-tdx");
    assert_eq!(ji["platform"]["tdx_enabled"], true);
    assert_eq!(ji["platform"]["qgs"]["required"], true);
    assert_eq!(ji["platform"]["qgs"]["unit_active"], true);
    assert_eq!(ji["platform"]["qgs"]["pccs_unit_active"], true);
    assert_eq!(ji["nat"]["uplink"], "eno1np0");
    assert_eq!(ji["ready_for_vm"], true);
}

#[test]
fn host_setup_is_zero_write_noop_on_configured_host_and_refuses_unconfigured_host() {
    let amd = HostFixture::build("amd");
    let ok = Command::new(OUTBE_UP_BIN)
        .env("PATH", amd.path_env())
        .args(["host", "setup", "--vmm-socket"])
        .arg(&amd.vmm_socket)
        .arg("--vmm-home")
        .arg(&amd.vmm_home)
        .arg("--sys-root")
        .arg(&amd.sys_root)
        .arg("--json")
        .output()
        .unwrap();
    assert!(
        ok.status.success(),
        "host setup on AMD fixture failed:\n{}",
        String::from_utf8_lossy(&ok.stderr)
    );
    let json: Value = serde_json::from_slice(&ok.stdout).unwrap();
    assert_eq!(json["already_configured"], true);
    assert_eq!(json["writes_performed"], 0);
    assert!(
        amd.mutations().is_empty(),
        "host setup must make zero writes"
    );

    let empty = HostFixture::build("unconfigured");
    let fail = Command::new(OUTBE_UP_BIN)
        .env("PATH", empty.path_env())
        .args(["host", "setup", "--vmm-socket"])
        .arg(&empty.vmm_socket)
        .arg("--vmm-home")
        .arg(&empty.vmm_home)
        .arg("--sys-root")
        .arg(&empty.sys_root)
        .arg("--json")
        .output()
        .unwrap();
    assert!(
        !fail.status.success(),
        "unconfigured host setup must refuse"
    );
    let fjson: Value = serde_json::from_slice(&fail.stdout).unwrap();
    assert_eq!(fjson["already_configured"], false);
    assert_eq!(fjson["writes_performed"], 0);
    assert_eq!(fjson["missing_components"].as_array().unwrap().len(), 3);
    assert_eq!(fjson["planned_steps"].as_array().unwrap().len(), 3);
    assert!(fjson["approval_note"]
        .as_str()
        .unwrap()
        .contains("owner design approval"));
    assert!(
        empty.mutations().is_empty(),
        "unconfigured host setup must perform zero writes"
    );
}

#[test]
fn host_nat_is_strictly_readonly_noop_or_prints_recommendations_and_remove_commands() {
    let amd = HostFixture::build("amd");
    let noop = Command::new(OUTBE_UP_BIN)
        .env("PATH", amd.path_env())
        .args(["host", "nat", "--bridge", "obr0", "--sys-root"])
        .arg(&amd.sys_root)
        .arg("--json")
        .output()
        .unwrap();
    assert!(
        noop.status.success(),
        "host nat failed:\n{}",
        String::from_utf8_lossy(&noop.stderr)
    );
    let j: Value = serde_json::from_slice(&noop.stdout).unwrap();
    assert_eq!(j["outcome"], "no_op_existing_egress");
    assert_eq!(j["writes_performed"], 0);
    assert!(
        j["recommended_commands"].as_array().unwrap().is_empty(),
        "no-op should not recommend commands"
    );
    assert!(
        amd.mutations().is_empty(),
        "host nat no-op must perform zero writes"
    );

    // Clean host without foreign egress: prints ruleset, unit, sysctl drop-in, and apply command with ZERO writes.
    let clean = HostFixture::build("clean_nat");
    let rec = Command::new(OUTBE_UP_BIN)
        .env("PATH", clean.path_env())
        .args(["host", "nat", "--bridge", "obr0", "--sys-root"])
        .arg(&clean.sys_root)
        .arg("--json")
        .output()
        .unwrap();
    assert!(
        rec.status.success(),
        "host nat recommend failed:\n{}",
        String::from_utf8_lossy(&rec.stderr)
    );
    let dj: Value = serde_json::from_slice(&rec.stdout).unwrap();
    assert_eq!(dj["outcome"], "print_only");
    assert_eq!(dj["writes_performed"], 0);
    assert!(dj["rendered_ruleset"]
        .as_str()
        .unwrap()
        .contains("table ip outbe_up_nat"));
    assert!(dj["rendered_unit"].as_str().unwrap().contains("outbe-up"));
    assert!(dj["rendered_sysctl"]
        .as_str()
        .unwrap()
        .contains("net.ipv4.ip_forward = 1"));
    assert_eq!(dj["recommended_commands"].as_array().unwrap().len(), 1);
    assert!(
        clean.mutations().is_empty(),
        "host nat must NEVER perform writes"
    );

    // `--remove` prints removal commands without deleting owned or foreign files (zero writes).
    let owned_rules = amd.sys_root.join("etc/outbe-up/nat.nft");
    write_file(
        &owned_rules,
        "# managed by outbe-up vm host-nat\ntable ip outbe_up_nat {}\n",
    );
    let rm = Command::new(OUTBE_UP_BIN)
        .env("PATH", amd.path_env())
        .args(["host", "nat", "--bridge", "obr0", "--remove", "--sys-root"])
        .arg(&amd.sys_root)
        .arg("--json")
        .output()
        .unwrap();
    assert!(
        rm.status.success(),
        "host nat remove failed:\n{}",
        String::from_utf8_lossy(&rm.stderr)
    );
    let rj: Value = serde_json::from_slice(&rm.stdout).unwrap();
    assert_eq!(rj["outcome"], "print_only");
    assert_eq!(rj["writes_performed"], 0);
    assert_eq!(rj["recommended_commands"].as_array().unwrap().len(), 1);
    assert!(
        owned_rules.exists(),
        "read-only host nat --remove must NOT delete files"
    );
    assert!(
        amd.sys_root.join("etc/sysctl.d/90-vm-host.conf").is_file(),
        "foreign sysctl file must remain untouched"
    );
    assert!(
        amd.mutations().is_empty(),
        "host nat --remove must perform zero writes"
    );
}

#[test]
fn host_reserve_and_release_are_strictly_readonly_and_refuse_conflicts() {
    let amd = HostFixture::build("amd");
    amd.seed_vm("node-a", "52:54:00:aa:bb:88", None);

    // 1. `--apply` is rejected on both `host reserve` and `host release`.
    let apply_rejected = Command::new(OUTBE_UP_BIN)
        .env("PATH", amd.path_env())
        .args([
            "host",
            "reserve",
            "--vm",
            "node-a",
            "--ip",
            "10.77.0.88",
            "--apply",
        ])
        .output()
        .unwrap();
    assert!(
        !apply_rejected.status.success(),
        "host reserve --apply must be rejected"
    );

    // 2. `host reserve` prints exact block + reload command with ZERO writes.
    let preview = Command::new(OUTBE_UP_BIN)
        .env("PATH", amd.path_env())
        .args([
            "host",
            "reserve",
            "--vm",
            "node-a",
            "--ip",
            "10.77.0.88",
            "--vmm-socket",
        ])
        .arg(&amd.vmm_socket)
        .arg("--vmm-home")
        .arg(&amd.vmm_home)
        .arg("--state-dir")
        .arg(&amd.state_dir)
        .arg("--sys-root")
        .arg(&amd.sys_root)
        .arg("--json")
        .output()
        .unwrap();
    assert!(
        preview.status.success(),
        "reserve preview failed:\n{}",
        String::from_utf8_lossy(&preview.stderr)
    );
    let pj: Value = serde_json::from_slice(&preview.stdout).unwrap();
    assert_eq!(pj["already_present"], false);
    assert_eq!(pj["writes_performed"], 0);
    assert_eq!(
        pj["block_content"],
        "# outbe-up:vm:node-a:begin\n52:54:00:aa:bb:88,10.77.0.88,10m\n# outbe-up:vm:node-a:end\n"
    );
    assert_eq!(
        pj["reload_command"],
        "sudo systemctl reload validator-vm-dhcp.service"
    );
    assert_eq!(
        fs::read_to_string(&amd.hosts_file).unwrap(),
        AMD_DHCP_HOSTS,
        "host reserve must NOT modify dhcp-hosts"
    );
    assert!(
        amd.mutations().is_empty(),
        "host reserve must NOT reload dhcp service"
    );

    // 3. Conflict refusals (legacy `.11`, active lease `.68`, gateway `.1`, outside subnet, wrong owner).
    for (bad_ip, expect_msg) in [
        ("10.77.0.11", "conflicts with existing dnsmasq hosts"),
        ("10.77.0.68", "conflicts with existing dnsmasq hosts"),
        ("10.77.0.1", "gateway address"),
        ("192.168.1.5", "outside bridge"),
    ] {
        let bad = Command::new(OUTBE_UP_BIN)
            .env("PATH", amd.path_env())
            .args([
                "host",
                "reserve",
                "--vm",
                "node-a",
                "--ip",
                bad_ip,
                "--vmm-socket",
            ])
            .arg(&amd.vmm_socket)
            .arg("--vmm-home")
            .arg(&amd.vmm_home)
            .arg("--state-dir")
            .arg(&amd.state_dir)
            .arg("--sys-root")
            .arg(&amd.sys_root)
            .output()
            .unwrap();
        assert!(!bad.status.success(), "expected refusal for {bad_ip}");
        let err = String::from_utf8_lossy(&bad.stderr);
        assert!(err.contains(expect_msg), "for {bad_ip}: {err}");
    }

    if rustix::process::getuid().as_raw() != 0 {
        let wrong_owner = Command::new(OUTBE_UP_BIN)
            .env("PATH", amd.path_env())
            .args([
                "host",
                "reserve",
                "--vm",
                "node-a",
                "--ip",
                "10.77.0.88",
                "--expected-hosts-owner",
                "0:0",
                "--vmm-socket",
            ])
            .arg(&amd.vmm_socket)
            .arg("--vmm-home")
            .arg(&amd.vmm_home)
            .arg("--state-dir")
            .arg(&amd.state_dir)
            .arg("--sys-root")
            .arg(&amd.sys_root)
            .output()
            .unwrap();
        assert!(!wrong_owner.status.success());
        assert!(String::from_utf8_lossy(&wrong_owner.stderr).contains("expected owner 0:0"));
    }

    // 4. After operator manually adds the printed block, `host reserve` reports `already_present=true`
    //    and `host release` reports `block_present=true` + prints exact block to remove (zero writes).
    let with_block = format!(
        "{AMD_DHCP_HOSTS}# outbe-up:vm:node-a:begin\n52:54:00:aa:bb:88,10.77.0.88,10m\n# outbe-up:vm:node-a:end\n"
    );
    fs::write(&amd.hosts_file, &with_block).unwrap();

    let already = Command::new(OUTBE_UP_BIN)
        .env("PATH", amd.path_env())
        .args([
            "host",
            "reserve",
            "--vm",
            "node-a",
            "--ip",
            "10.77.0.88",
            "--vmm-socket",
        ])
        .arg(&amd.vmm_socket)
        .arg("--vmm-home")
        .arg(&amd.vmm_home)
        .arg("--state-dir")
        .arg(&amd.state_dir)
        .arg("--sys-root")
        .arg(&amd.sys_root)
        .arg("--json")
        .output()
        .unwrap();
    assert!(already.status.success());
    let aj: Value = serde_json::from_slice(&already.stdout).unwrap();
    assert_eq!(aj["already_present"], true);
    assert_eq!(aj["writes_performed"], 0);

    let rel_preview = Command::new(OUTBE_UP_BIN)
        .env("PATH", amd.path_env())
        .args(["host", "release", "--vm", "node-a", "--state-dir"])
        .arg(&amd.state_dir)
        .arg("--sys-root")
        .arg(&amd.sys_root)
        .arg("--json")
        .output()
        .unwrap();
    assert!(rel_preview.status.success());
    let rpj: Value = serde_json::from_slice(&rel_preview.stdout).unwrap();
    assert_eq!(rpj["block_present"], true);
    assert_eq!(rpj["writes_performed"], 0);
    assert!(rpj["existing_block"]
        .as_str()
        .unwrap()
        .contains("52:54:00:aa:bb:88,10.77.0.88,10m"));
    assert_eq!(
        fs::read_to_string(&amd.hosts_file).unwrap(),
        with_block,
        "host release must NOT modify dhcp-hosts"
    );
    assert!(amd.mutations().is_empty());

    // 5. Unrecognized file format and symlink at `dhcp-hosts` are both refused.
    fs::write(&amd.hosts_file, "corrupted non-dnsmasq content\n").unwrap();
    let bad_fmt = Command::new(OUTBE_UP_BIN)
        .env("PATH", amd.path_env())
        .args([
            "host",
            "reserve",
            "--vm",
            "node-a",
            "--ip",
            "10.77.0.88",
            "--vmm-socket",
        ])
        .arg(&amd.vmm_socket)
        .arg("--vmm-home")
        .arg(&amd.vmm_home)
        .arg("--state-dir")
        .arg(&amd.state_dir)
        .arg("--sys-root")
        .arg(&amd.sys_root)
        .output()
        .unwrap();
    assert!(!bad_fmt.status.success());
    assert!(String::from_utf8_lossy(&bad_fmt.stderr).contains("unrecognized"));

    fs::remove_file(&amd.hosts_file).unwrap();
    symlink(&amd.leases_file, &amd.hosts_file).unwrap();
    let sym_err = Command::new(OUTBE_UP_BIN)
        .env("PATH", amd.path_env())
        .args([
            "host",
            "reserve",
            "--vm",
            "node-a",
            "--ip",
            "10.77.0.88",
            "--vmm-socket",
        ])
        .arg(&amd.vmm_socket)
        .arg("--vmm-home")
        .arg(&amd.vmm_home)
        .arg("--state-dir")
        .arg(&amd.state_dir)
        .arg("--sys-root")
        .arg(&amd.sys_root)
        .output()
        .unwrap();
    assert!(!sym_err.status.success());
    assert!(String::from_utf8_lossy(&sym_err.stderr).contains("symlink"));
    assert!(amd.mutations().is_empty());
}

#[test]
fn host_publish_detects_port_9000_conflict_and_prints_valid_nft_rules_readonly() {
    let amd = HostFixture::build("amd");
    amd.seed_vm(
        "node-a",
        "52:54:00:aa:bb:88",
        Some(Ipv4Addr::new(10, 77, 0, 88)),
    );

    // 1. Port 9000 is already DNATed to 10.77.0.11 -> conflict detected, non-zero exit, zero writes.
    let conflict = Command::new(OUTBE_UP_BIN)
        .env("PATH", amd.path_env())
        .args([
            "host",
            "publish",
            "--vm",
            "node-a",
            "--port",
            "9000",
            "--source",
            "141.95.147.184/32",
            "--vmm-socket",
        ])
        .arg(&amd.vmm_socket)
        .arg("--vmm-home")
        .arg(&amd.vmm_home)
        .arg("--state-dir")
        .arg(&amd.state_dir)
        .arg("--sys-root")
        .arg(&amd.sys_root)
        .arg("--json")
        .output()
        .unwrap();
    assert!(
        !conflict.status.success(),
        "port 9000 conflict must fail non-zero"
    );
    let cj: Value = serde_json::from_slice(&conflict.stdout).unwrap();
    assert_eq!(cj["writes_performed"], 0);
    assert_eq!(cj["conflicts"].as_array().unwrap().len(), 1);
    assert_eq!(cj["conflicts"][0]["host_port"], 9000);
    assert_eq!(cj["conflicts"][0]["existing_dest_ip"], "10.77.0.11");
    assert_eq!(
        cj["conflicts"][0]["existing_comment"],
        "validator-0-hyperlane"
    );
    assert_eq!(cj["conflicts"][0]["existing_handle"], 456);
    assert!(String::from_utf8_lossy(&conflict.stderr).contains("port conflict"));

    // 2. Free host port `9000:9010` prints exact `/etc/host-firewall.nft` syntax + apply/remove steps, zero writes.
    let ok = Command::new(OUTBE_UP_BIN)
        .env("PATH", amd.path_env())
        .args([
            "host",
            "publish",
            "--vm",
            "node-a",
            "--port",
            "9000:9010",
            "--source",
            "141.95.147.184/32",
            "--vmm-socket",
        ])
        .arg(&amd.vmm_socket)
        .arg("--vmm-home")
        .arg(&amd.vmm_home)
        .arg("--state-dir")
        .arg(&amd.state_dir)
        .arg("--sys-root")
        .arg(&amd.sys_root)
        .arg("--json")
        .output()
        .unwrap();
    assert!(
        ok.status.success(),
        "host publish failed:\n{}",
        String::from_utf8_lossy(&ok.stderr)
    );
    let pj: Value = serde_json::from_slice(&ok.stdout).unwrap();
    assert_eq!(pj["writes_performed"], 0);
    assert!(pj["conflicts"].as_array().unwrap().is_empty());
    assert_eq!(
        pj["forward_rules"][0],
        r#"ip saddr { 141.95.147.184/32 } ip daddr 10.77.0.88 ct status dnat tcp dport 9000 accept comment "outbe-up:node-a";"#
    );
    assert_eq!(
        pj["prerouting_rules"][0],
        r#"ip daddr 162.19.107.121 ip saddr { 141.95.147.184/32 } tcp dport 9010 dnat to 10.77.0.88:9000 comment "outbe-up:node-a";"#
    );
    assert!(pj["apply_commands"].as_array().unwrap().iter().any(|v| v
        .as_str()
        .unwrap_or("")
        .contains("nft --check --file /etc/host-firewall.nft")));
    assert!(pj["remove_commands"].as_array().unwrap().iter().any(|v| v
        .as_str()
        .unwrap_or("")
        .contains(r#"comment "outbe-up:node-a""#)));
    assert!(
        amd.mutations().is_empty(),
        "host publish must NEVER perform any writes"
    );
}

#[test]
fn host_reserve_and_release_f1_f2_readonly_regressions() {
    let amd = HostFixture::build("amd");
    amd.seed_vm("node-a", "52:54:00:aa:bb:88", None);

    // F1: Foreign unmanaged line carrying our exact MAC+IP -> reserve refuses,
    //     foreign bytes remain untouched; release reports absent and leaves bytes untouched.
    let foreign_with_same = "\
52:54:00:aa:bb:88,10.77.0.88,10m
# outbe-up:foreign:begin
52:54:00:de:ad:01,10.77.0.50,10m
# outbe-up:foreign:end
";
    fs::write(&amd.hosts_file, foreign_with_same).unwrap();
    let f1_res = Command::new(OUTBE_UP_BIN)
        .env("PATH", amd.path_env())
        .args([
            "host",
            "reserve",
            "--vm",
            "node-a",
            "--ip",
            "10.77.0.88",
            "--vmm-socket",
        ])
        .arg(&amd.vmm_socket)
        .arg("--vmm-home")
        .arg(&amd.vmm_home)
        .arg("--state-dir")
        .arg(&amd.state_dir)
        .arg("--sys-root")
        .arg(&amd.sys_root)
        .output()
        .unwrap();
    assert!(
        !f1_res.status.success(),
        "F1: foreign line with our MAC/IP must refuse"
    );
    assert_eq!(
        fs::read_to_string(&amd.hosts_file).unwrap(),
        foreign_with_same
    );

    let f1_rel = Command::new(OUTBE_UP_BIN)
        .env("PATH", amd.path_env())
        .args(["host", "release", "--vm", "node-a", "--state-dir"])
        .arg(&amd.state_dir)
        .arg("--sys-root")
        .arg(&amd.sys_root)
        .arg("--json")
        .output()
        .unwrap();
    assert!(f1_rel.status.success());
    let rj: Value = serde_json::from_slice(&f1_rel.stdout).unwrap();
    assert_eq!(rj["block_present"], false);
    assert_eq!(rj["writes_performed"], 0);
    assert_eq!(
        fs::read_to_string(&amd.hosts_file).unwrap(),
        foreign_with_same
    );

    // F2: Unclosed marker AND foreign line inside our own block -> reserve and release BOTH refuse.
    let f2_cases = [
        "\
# outbe-up:vm:node-a:begin
52:54:00:aa:bb:88,10.77.0.88,10m
# outbe-up:final-amd:begin
be:f7:7b:2c:d1:85,10.77.0.68,10m
# outbe-up:final-amd:end
",
        "\
# outbe-up:vm:node-a:begin
52:54:00:aa:bb:88,10.77.0.88,10m
be:6a:63:f8:84:d0,10.77.0.11,validator-0,infinite
# outbe-up:vm:node-a:end
",
    ];
    for broken in f2_cases {
        fs::write(&amd.hosts_file, broken).unwrap();
        let res_bad = Command::new(OUTBE_UP_BIN)
            .env("PATH", amd.path_env())
            .args([
                "host",
                "reserve",
                "--vm",
                "node-a",
                "--ip",
                "10.77.0.88",
                "--vmm-socket",
            ])
            .arg(&amd.vmm_socket)
            .arg("--vmm-home")
            .arg(&amd.vmm_home)
            .arg("--state-dir")
            .arg(&amd.state_dir)
            .arg("--sys-root")
            .arg(&amd.sys_root)
            .output()
            .unwrap();
        assert!(
            !res_bad.status.success(),
            "F2: reserve must refuse broken markers/body"
        );
        assert_eq!(fs::read_to_string(&amd.hosts_file).unwrap(), broken);

        let rel_bad = Command::new(OUTBE_UP_BIN)
            .env("PATH", amd.path_env())
            .args(["host", "release", "--vm", "node-a", "--state-dir"])
            .arg(&amd.state_dir)
            .arg("--sys-root")
            .arg(&amd.sys_root)
            .output()
            .unwrap();
        assert!(
            !rel_bad.status.success(),
            "F2: release must refuse broken markers/body"
        );
        assert_eq!(fs::read_to_string(&amd.hosts_file).unwrap(), broken);
    }
    assert!(amd.mutations().is_empty());
}

#[test]
fn host_publish_f4_port_ranges_sets_and_unparsed_dnat_fail_closed() {
    let amd = HostFixture::build("amd");
    amd.seed_vm(
        "node-a",
        "52:54:00:aa:bb:88",
        Some(Ipv4Addr::new(10, 77, 0, 88)),
    );
    let nft_file = amd._tmp.path().join("nft.json");

    // 1. Port range 9000-9010 + set {9020, 9021} -> conflicts on 9005 and 9021, free on 9011.
    let range_and_set_nft = r#"{"nftables":[
      {"metainfo":{"version":"1.1.6","json_schema_version":1}},
      {"table":{"family":"inet","name":"host_firewall","handle":66}},
      {"chain":{"family":"inet","table":"host_firewall","name":"forward","handle":2,"type":"filter","hook":"forward","prio":0,"policy":"drop"}},
      {"table":{"family":"ip","name":"host_firewall_nat","handle":67}},
      {"chain":{"family":"ip","table":"host_firewall_nat","name":"prerouting","handle":1,"type":"nat","hook":"prerouting","prio":-100,"policy":"accept"}},
      {"rule":{"family":"ip","table":"host_firewall_nat","chain":"prerouting","handle":401,"comment":"range-rule","expr":[
        {"match":{"op":"==","left":{"payload":{"protocol":"ip","field":"daddr"}},"right":"162.19.107.121"}},
        {"match":{"op":"==","left":{"payload":{"protocol":"tcp","field":"dport"}},"right":{"range":[9000,9010]}}},
        {"dnat":{"addr":"10.77.0.11"}}
      ]}},
      {"rule":{"family":"ip","table":"host_firewall_nat","chain":"prerouting","handle":402,"comment":"set-rule","expr":[
        {"match":{"op":"==","left":{"payload":{"protocol":"ip","field":"daddr"}},"right":"162.19.107.121"}},
        {"match":{"op":"==","left":{"payload":{"protocol":"tcp","field":"dport"}},"right":{"set":[9020,9021]}}},
        {"dnat":{"addr":"10.77.0.12"}}
      ]}}
    ]}"#;
    fs::write(&nft_file, range_and_set_nft).unwrap();

    for conflict_port in ["9005", "9021"] {
        let out = Command::new(OUTBE_UP_BIN)
            .env("PATH", amd.path_env())
            .args([
                "host",
                "publish",
                "--vm",
                "node-a",
                "--port",
                conflict_port,
                "--vmm-socket",
            ])
            .arg(&amd.vmm_socket)
            .arg("--vmm-home")
            .arg(&amd.vmm_home)
            .arg("--state-dir")
            .arg(&amd.state_dir)
            .arg("--sys-root")
            .arg(&amd.sys_root)
            .arg("--json")
            .output()
            .unwrap();
        assert!(
            !out.status.success(),
            "F4: port {conflict_port} inside range/set must conflict"
        );
        let j: Value = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(j["conflicts"].as_array().unwrap().len(), 1);
    }

    let ok_free = Command::new(OUTBE_UP_BIN)
        .env("PATH", amd.path_env())
        .args([
            "host",
            "publish",
            "--vm",
            "node-a",
            "--port",
            "9000:9011",
            "--vmm-socket",
        ])
        .arg(&amd.vmm_socket)
        .arg("--vmm-home")
        .arg(&amd.vmm_home)
        .arg("--state-dir")
        .arg(&amd.state_dir)
        .arg("--sys-root")
        .arg(&amd.sys_root)
        .arg("--json")
        .output()
        .unwrap();
    assert!(
        ok_free.status.success(),
        "free port 9011 should succeed:\n{}",
        String::from_utf8_lossy(&ok_free.stderr)
    );

    // 2. Unparseable DNAT rule (named set `@ports` / DNAT map) -> recorded in status AND fails closed in publish.
    let unparsed_nft = r#"{"nftables":[
      {"metainfo":{"version":"1.1.6","json_schema_version":1}},
      {"table":{"family":"ip","name":"host_firewall_nat","handle":67}},
      {"chain":{"family":"ip","table":"host_firewall_nat","name":"prerouting","handle":1,"type":"nat","hook":"prerouting","prio":-100,"policy":"accept"}},
      {"rule":{"family":"ip","table":"host_firewall_nat","chain":"prerouting","handle":999,"comment":"dynamic-dnat","expr":[
        {"match":{"op":"==","left":{"payload":{"protocol":"tcp","field":"dport"}},"right":"@dynamic_ports"}},
        {"dnat":{"addr":"10.77.0.11"}}
      ]}}
    ]}"#;
    fs::write(&nft_file, unparsed_nft).unwrap();

    let st = Command::new(OUTBE_UP_BIN)
        .env("PATH", amd.path_env())
        .args(["host", "status", "--vmm-socket"])
        .arg(&amd.vmm_socket)
        .arg("--vmm-home")
        .arg(&amd.vmm_home)
        .arg("--sys-root")
        .arg(&amd.sys_root)
        .arg("--json")
        .output()
        .unwrap();
    assert!(st.status.success());
    let sj: Value = serde_json::from_slice(&st.stdout).unwrap();
    assert_eq!(
        sj["firewall"]["unparsed_dnat_rules"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(sj["firewall"]["unparsed_dnat_rules"][0]["handle"], 999);

    let fail_closed = Command::new(OUTBE_UP_BIN)
        .env("PATH", amd.path_env())
        .args([
            "host",
            "publish",
            "--vm",
            "node-a",
            "--port",
            "9050",
            "--vmm-socket",
        ])
        .arg(&amd.vmm_socket)
        .arg("--vmm-home")
        .arg(&amd.vmm_home)
        .arg("--state-dir")
        .arg(&amd.state_dir)
        .arg("--sys-root")
        .arg(&amd.sys_root)
        .output()
        .unwrap();
    assert!(
        !fail_closed.status.success(),
        "F4: unparseable DNAT rule must fail closed in host publish"
    );
    assert!(
        String::from_utf8_lossy(&fail_closed.stderr).contains("unparseable DNAT"),
        "stderr: {}",
        String::from_utf8_lossy(&fail_closed.stderr)
    );
    assert!(amd.mutations().is_empty());
}

#[test]
fn host_check_verifies_network_dhcp_and_published_ports_readonly() {
    let amd = HostFixture::build("amd");
    amd.seed_vm(
        "node-a",
        "52:54:00:aa:bb:88",
        Some(Ipv4Addr::new(10, 77, 0, 88)),
    );
    let nft_file = amd._tmp.path().join("nft.json");

    // 1. Missing static DHCP reservation -> non-zero exit, ready=false, prints exact recommendation, zero writes.
    let missing_dhcp = Command::new(OUTBE_UP_BIN)
        .env("PATH", amd.path_env())
        .args(["host", "check", "--vm", "node-a", "--vmm-socket"])
        .arg(&amd.vmm_socket)
        .arg("--vmm-home")
        .arg(&amd.vmm_home)
        .arg("--state-dir")
        .arg(&amd.state_dir)
        .arg("--sys-root")
        .arg(&amd.sys_root)
        .arg("--json")
        .output()
        .unwrap();
    assert!(
        !missing_dhcp.status.success(),
        "host check must fail when static DHCP reservation is missing"
    );
    let mj: Value = serde_json::from_slice(&missing_dhcp.stdout).unwrap();
    assert_eq!(mj["ready"], false);
    assert_eq!(mj["host_network_ok"], true);
    assert_eq!(mj["dhcp_reserved"], false);
    assert_eq!(mj["published_ports_ok"], true);
    assert_eq!(mj["writes_performed"], 0);
    assert_eq!(
        mj["recommended_dhcp_block"],
        "# outbe-up:vm:node-a:begin\n52:54:00:aa:bb:88,10.77.0.88,10m\n# outbe-up:vm:node-a:end\n"
    );
    assert_eq!(
        mj["recommended_dhcp_reload"],
        "sudo systemctl reload validator-vm-dhcp.service"
    );
    assert!(amd.mutations().is_empty());

    // 2. Operator adds `# outbe-up:vm:node-a:begin..end` -> host check (no ports) passes!
    let with_block = format!(
        "{AMD_DHCP_HOSTS}# outbe-up:vm:node-a:begin\n52:54:00:aa:bb:88,10.77.0.88,10m\n# outbe-up:vm:node-a:end\n"
    );
    fs::write(&amd.hosts_file, &with_block).unwrap();

    let ok_managed = Command::new(OUTBE_UP_BIN)
        .env("PATH", amd.path_env())
        .args(["host", "check", "--vm", "node-a", "--vmm-socket"])
        .arg(&amd.vmm_socket)
        .arg("--vmm-home")
        .arg(&amd.vmm_home)
        .arg("--state-dir")
        .arg(&amd.state_dir)
        .arg("--sys-root")
        .arg(&amd.sys_root)
        .arg("--json")
        .output()
        .unwrap();
    assert!(
        ok_managed.status.success(),
        "host check with managed DHCP block failed:\n{}",
        String::from_utf8_lossy(&ok_managed.stderr)
    );
    let okj: Value = serde_json::from_slice(&ok_managed.stdout).unwrap();
    assert_eq!(okj["ready"], true);
    assert_eq!(okj["host_network_ok"], true);
    assert_eq!(okj["dhcp_reserved"], true);
    assert_eq!(okj["published_ports_ok"], true);
    assert_eq!(okj["writes_performed"], 0);

    // 3. Operator uses a plain static line instead of managed markers -> also accepted!
    let with_plain = format!("{AMD_DHCP_HOSTS}52:54:00:aa:bb:88,10.77.0.88,node-a,infinite\n");
    fs::write(&amd.hosts_file, &with_plain).unwrap();
    let ok_plain = Command::new(OUTBE_UP_BIN)
        .env("PATH", amd.path_env())
        .args(["host", "check", "--vm", "node-a", "--vmm-socket"])
        .arg(&amd.vmm_socket)
        .arg("--vmm-home")
        .arg(&amd.vmm_home)
        .arg("--state-dir")
        .arg(&amd.state_dir)
        .arg("--sys-root")
        .arg(&amd.sys_root)
        .arg("--json")
        .output()
        .unwrap();
    assert!(
        ok_plain.status.success(),
        "host check with operator plain DHCP line failed:\n{}",
        String::from_utf8_lossy(&ok_plain.stderr)
    );
    let pj: Value = serde_json::from_slice(&ok_plain.stdout).unwrap();
    assert_eq!(pj["ready"], true);
    assert_eq!(pj["dhcp_reserved"], true);

    // 4. DHCP IP conflict (different MAC owns 10.77.0.88) -> fails closed with conflict!
    let with_conflict = format!("{AMD_DHCP_HOSTS}52:54:00:de:ad:99,10.77.0.88,other-vm,infinite\n");
    fs::write(&amd.hosts_file, &with_conflict).unwrap();
    let bad_dhcp = Command::new(OUTBE_UP_BIN)
        .env("PATH", amd.path_env())
        .args(["host", "check", "--vm", "node-a", "--vmm-socket"])
        .arg(&amd.vmm_socket)
        .arg("--vmm-home")
        .arg(&amd.vmm_home)
        .arg("--state-dir")
        .arg(&amd.state_dir)
        .arg("--sys-root")
        .arg(&amd.sys_root)
        .arg("--json")
        .output()
        .unwrap();
    assert!(!bad_dhcp.status.success());
    let bcj: Value = serde_json::from_slice(&bad_dhcp.stdout).unwrap();
    assert_eq!(bcj["ready"], false);
    assert_eq!(bcj["dhcp_reserved"], false);
    assert!(!bcj["conflicts"].as_array().unwrap().is_empty());

    // Restore valid DHCP reservation for port publication checks.
    fs::write(&amd.hosts_file, &with_block).unwrap();

    // 5. Missing published ports -> ready=false, prints exact forward + prerouting rules + apply commands.
    let missing_ports = Command::new(OUTBE_UP_BIN)
        .env("PATH", amd.path_env())
        .args([
            "host",
            "check",
            "--vm",
            "node-a",
            "--port",
            "30303:30313",
            "--port",
            "30303:30313/udp",
            "--source",
            "141.95.168.110/32",
            "--vmm-socket",
        ])
        .arg(&amd.vmm_socket)
        .arg("--vmm-home")
        .arg(&amd.vmm_home)
        .arg("--state-dir")
        .arg(&amd.state_dir)
        .arg("--sys-root")
        .arg(&amd.sys_root)
        .arg("--json")
        .output()
        .unwrap();
    assert!(!missing_ports.status.success());
    let mpj: Value = serde_json::from_slice(&missing_ports.stdout).unwrap();
    assert_eq!(mpj["ready"], false);
    assert_eq!(mpj["dhcp_reserved"], true);
    assert_eq!(mpj["published_ports_ok"], false);
    assert_eq!(
        mpj["recommended_forward_rules"].as_array().unwrap().len(),
        2
    );
    assert_eq!(
        mpj["recommended_prerouting_rules"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert!(!mpj["recommended_firewall_commands"]
        .as_array()
        .unwrap()
        .is_empty());

    // 6. DNAT present in prerouting WITHOUT forward accept rule -> MUST still fail!
    let dnat_only_nft = r#"{"nftables":[
      {"metainfo":{"version":"1.1.6","json_schema_version":1}},
      {"table":{"family":"inet","name":"host_firewall","handle":66}},
      {"chain":{"family":"inet","table":"host_firewall","name":"forward","handle":2,"type":"filter","hook":"forward","prio":0,"policy":"drop"}},
      {"rule":{"family":"inet","table":"host_firewall","chain":"forward","handle":700,"comment":"outbound:validator_vms","expr":[
        {"match":{"op":"==","left":{"payload":{"protocol":"ip","field":"saddr"}},"right":{"prefix":{"addr":"10.77.0.0","len":24}}}},
        {"accept":null}
      ]}},
      {"table":{"family":"ip","name":"host_firewall_nat","handle":67}},
      {"chain":{"family":"ip","table":"host_firewall_nat","name":"prerouting","handle":1,"type":"nat","hook":"prerouting","prio":-100,"policy":"accept"}},
      {"chain":{"family":"ip","table":"host_firewall_nat","name":"postrouting","handle":2,"type":"nat","hook":"postrouting","prio":100,"policy":"accept"}},
      {"rule":{"family":"ip","table":"host_firewall_nat","chain":"prerouting","handle":801,"comment":"outbe-up:node-a","expr":[
        {"match":{"op":"==","left":{"payload":{"protocol":"ip","field":"daddr"}},"right":"162.19.107.121"}},
        {"match":{"op":"==","left":{"payload":{"protocol":"ip","field":"saddr"}},"right":"141.95.168.110"}},
        {"match":{"op":"==","left":{"payload":{"protocol":"tcp","field":"dport"}},"right":30313}},
        {"dnat":{"addr":"10.77.0.88","port":30303}}
      ]}},
      {"rule":{"family":"ip","table":"host_firewall_nat","chain":"postrouting","handle":468,"comment":"outbound:validator_vms","expr":[
        {"match":{"op":"==","left":{"payload":{"protocol":"ip","field":"saddr"}},"right":{"prefix":{"addr":"10.77.0.0","len":24}}}},
        {"match":{"op":"!=","left":{"payload":{"protocol":"ip","field":"daddr"}},"right":{"prefix":{"addr":"10.77.0.0","len":24}}}},
        {"masquerade":null}
      ]}}
    ]}"#;
    fs::write(&nft_file, dnat_only_nft).unwrap();
    let dnat_only = Command::new(OUTBE_UP_BIN)
        .env("PATH", amd.path_env())
        .args([
            "host",
            "check",
            "--vm",
            "node-a",
            "--port",
            "30303:30313",
            "--vmm-socket",
        ])
        .arg(&amd.vmm_socket)
        .arg("--vmm-home")
        .arg(&amd.vmm_home)
        .arg("--state-dir")
        .arg(&amd.state_dir)
        .arg("--sys-root")
        .arg(&amd.sys_root)
        .arg("--json")
        .output()
        .unwrap();
    assert!(
        !dnat_only.status.success(),
        "host check must fail when DNAT exists without forward accept rule"
    );
    let doj: Value = serde_json::from_slice(&dnat_only.stdout).unwrap();
    assert_eq!(doj["ready"], false);
    assert_eq!(doj["published_ports_ok"], false);
    assert_eq!(
        doj["recommended_prerouting_rules"]
            .as_array()
            .unwrap()
            .len(),
        0
    );
    assert_eq!(
        doj["recommended_forward_rules"].as_array().unwrap().len(),
        1
    );

    // 7. BOTH DNAT + forward accept present -> passes (and enforces explicit `--source` match)!
    let both_nft = r#"{"nftables":[
      {"metainfo":{"version":"1.1.6","json_schema_version":1}},
      {"table":{"family":"inet","name":"host_firewall","handle":66}},
      {"chain":{"family":"inet","table":"host_firewall","name":"forward","handle":2,"type":"filter","hook":"forward","prio":0,"policy":"drop"}},
      {"rule":{"family":"inet","table":"host_firewall","chain":"forward","handle":700,"comment":"outbound:validator_vms","expr":[
        {"match":{"op":"==","left":{"payload":{"protocol":"ip","field":"saddr"}},"right":{"prefix":{"addr":"10.77.0.0","len":24}}}},
        {"accept":null}
      ]}},
      {"rule":{"family":"inet","table":"host_firewall","chain":"forward","handle":811,"comment":"outbe-up:node-a","expr":[
        {"match":{"op":"==","left":{"payload":{"protocol":"ip","field":"saddr"}},"right":"141.95.168.110"}},
        {"match":{"op":"==","left":{"payload":{"protocol":"ip","field":"daddr"}},"right":"10.77.0.88"}},
        {"match":{"op":"in","left":{"ct":{"key":"status"}},"right":"dnat"}},
        {"match":{"op":"==","left":{"payload":{"protocol":"tcp","field":"dport"}},"right":30303}},
        {"accept":null}
      ]}},
      {"table":{"family":"ip","name":"host_firewall_nat","handle":67}},
      {"chain":{"family":"ip","table":"host_firewall_nat","name":"prerouting","handle":1,"type":"nat","hook":"prerouting","prio":-100,"policy":"accept"}},
      {"chain":{"family":"ip","table":"host_firewall_nat","name":"postrouting","handle":2,"type":"nat","hook":"postrouting","prio":100,"policy":"accept"}},
      {"rule":{"family":"ip","table":"host_firewall_nat","chain":"prerouting","handle":801,"comment":"outbe-up:node-a","expr":[
        {"match":{"op":"==","left":{"payload":{"protocol":"ip","field":"daddr"}},"right":"162.19.107.121"}},
        {"match":{"op":"==","left":{"payload":{"protocol":"ip","field":"saddr"}},"right":"141.95.168.110"}},
        {"match":{"op":"==","left":{"payload":{"protocol":"tcp","field":"dport"}},"right":30313}},
        {"dnat":{"addr":"10.77.0.88","port":30303}}
      ]}},
      {"rule":{"family":"ip","table":"host_firewall_nat","chain":"postrouting","handle":468,"comment":"outbound:validator_vms","expr":[
        {"match":{"op":"==","left":{"payload":{"protocol":"ip","field":"saddr"}},"right":{"prefix":{"addr":"10.77.0.0","len":24}}}},
        {"match":{"op":"!=","left":{"payload":{"protocol":"ip","field":"daddr"}},"right":{"prefix":{"addr":"10.77.0.0","len":24}}}},
        {"masquerade":null}
      ]}}
    ]}"#;
    fs::write(&nft_file, both_nft).unwrap();
    let both_ok = Command::new(OUTBE_UP_BIN)
        .env("PATH", amd.path_env())
        .args([
            "host",
            "check",
            "--vm",
            "node-a",
            "--port",
            "30303:30313",
            "--source",
            "141.95.168.110",
            "--vmm-socket",
        ])
        .arg(&amd.vmm_socket)
        .arg("--vmm-home")
        .arg(&amd.vmm_home)
        .arg("--state-dir")
        .arg(&amd.state_dir)
        .arg("--sys-root")
        .arg(&amd.sys_root)
        .arg("--json")
        .output()
        .unwrap();
    assert!(
        both_ok.status.success(),
        "host check with both DNAT and forward accept failed:\n{}",
        String::from_utf8_lossy(&both_ok.stderr)
    );
    let boj: Value = serde_json::from_slice(&both_ok.stdout).unwrap();
    assert_eq!(boj["ready"], true);
    assert_eq!(boj["published_ports_ok"], true);
    assert_eq!(boj["writes_performed"], 0);

    // Requesting an uncovered source CIDR fails!
    let wrong_src = Command::new(OUTBE_UP_BIN)
        .env("PATH", amd.path_env())
        .args([
            "host",
            "check",
            "--vm",
            "node-a",
            "--port",
            "30303:30313",
            "--source",
            "203.0.113.9/32",
            "--vmm-socket",
        ])
        .arg(&amd.vmm_socket)
        .arg("--vmm-home")
        .arg(&amd.vmm_home)
        .arg("--state-dir")
        .arg(&amd.state_dir)
        .arg("--sys-root")
        .arg(&amd.sys_root)
        .arg("--json")
        .output()
        .unwrap();
    assert!(
        !wrong_src.status.success(),
        "host check must fail when requested --source is not allowed"
    );

    // 8. Unparseable DNAT rule -> fails closed!
    let unparsed_nft = r#"{"nftables":[
      {"metainfo":{"version":"1.1.6","json_schema_version":1}},
      {"table":{"family":"ip","name":"host_firewall_nat","handle":67}},
      {"chain":{"family":"ip","table":"host_firewall_nat","name":"prerouting","handle":1,"type":"nat","hook":"prerouting","prio":-100,"policy":"accept"}},
      {"chain":{"family":"ip","table":"host_firewall_nat","name":"postrouting","handle":2,"type":"nat","hook":"postrouting","prio":100,"policy":"accept"}},
      {"rule":{"family":"ip","table":"host_firewall_nat","chain":"prerouting","handle":999,"comment":"dynamic","expr":[
        {"match":{"op":"==","left":{"payload":{"protocol":"tcp","field":"dport"}},"right":"@dynamic_ports"}},
        {"dnat":{"addr":"10.77.0.11"}}
      ]}},
      {"rule":{"family":"ip","table":"host_firewall_nat","chain":"postrouting","handle":468,"comment":"outbound:validator_vms","expr":[
        {"match":{"op":"==","left":{"payload":{"protocol":"ip","field":"saddr"}},"right":{"prefix":{"addr":"10.77.0.0","len":24}}}},
        {"match":{"op":"!=","left":{"payload":{"protocol":"ip","field":"daddr"}},"right":{"prefix":{"addr":"10.77.0.0","len":24}}}},
        {"masquerade":null}
      ]}}
    ]}"#;
    fs::write(&nft_file, unparsed_nft).unwrap();
    let unparsed = Command::new(OUTBE_UP_BIN)
        .env("PATH", amd.path_env())
        .args([
            "host",
            "check",
            "--vm",
            "node-a",
            "--port",
            "30303:30313",
            "--vmm-socket",
        ])
        .arg(&amd.vmm_socket)
        .arg("--vmm-home")
        .arg(&amd.vmm_home)
        .arg("--state-dir")
        .arg(&amd.state_dir)
        .arg("--sys-root")
        .arg(&amd.sys_root)
        .arg("--json")
        .output()
        .unwrap();
    assert!(!unparsed.status.success());
    let uj: Value = serde_json::from_slice(&unparsed.stdout).unwrap();
    assert_eq!(uj["ready"], false);
    assert_eq!(uj["published_ports_ok"], false);
    assert!(uj["conflicts"][0]
        .as_str()
        .unwrap()
        .contains("unparseable DNAT"));

    // 9. Unconfigured host -> fails with host_network_ok=false and zero writes.
    let empty = HostFixture::build("unconfigured");
    empty.seed_vm(
        "node-a",
        "52:54:00:aa:bb:88",
        Some(Ipv4Addr::new(10, 77, 0, 88)),
    );
    let unconf = Command::new(OUTBE_UP_BIN)
        .env("PATH", empty.path_env())
        .args(["host", "check", "--vm", "node-a", "--vmm-socket"])
        .arg(&empty.vmm_socket)
        .arg("--vmm-home")
        .arg(&empty.vmm_home)
        .arg("--state-dir")
        .arg(&empty.state_dir)
        .arg("--sys-root")
        .arg(&empty.sys_root)
        .arg("--json")
        .output()
        .unwrap();
    assert!(!unconf.status.success());
    let ej: Value = serde_json::from_slice(&unconf.stdout).unwrap();
    assert_eq!(ej["ready"], false);
    assert_eq!(ej["host_network_ok"], false);
    assert_eq!(ej["writes_performed"], 0);
    assert!(amd.mutations().is_empty());
    assert!(empty.mutations().is_empty());
}
