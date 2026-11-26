//! Durable per-network VM desired/cached state (`<artifacts>/network-vm-state.json`, schema v2).
//!
//! Host `outbe-up vm status --json` is always the source of truth for VM existence and lifecycle.
//! This file stores NO VM phase journal; it records only:
//! - Network identity (`network_name`, `chain_id`, `genesis_sha256`, `image_source_id`, `outbe_up_sha256`)
//! - Deployed guest image metadata (`image_name`, `image_hash`)
//! - Extensible `members[]` (with `role: founder | joining-validator | full-node`) carrying each
//!   member's `owner_tag`, `image_hash`, prepared `disk_source_sha256` (for staging reuse),
//!   `no_tee`, and advisory `vm_id`/`mac`.
//!
//! All reads/writes are serialized under an exclusive `O_CLOEXEC` `flock` on
//! `<artifacts>/.network-vm-state.lock` and committed atomically via `0600` temp file + fsync +
//! rename.

use std::{
    fs,
    io::Write,
    net::Ipv4Addr,
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
};

use eyre::{ensure, Context, Result};
use rustix::fs::{flock, openat, FlockOperation, Mode, OFlags, CWD};
use serde::{Deserialize, Serialize};

use super::{
    config::{Ssh, VmFlavor},
    remote::NetworkArtifacts,
};

pub const NETWORK_VM_STATE_VERSION: u32 = 2;
/// `validator.env` schema version written on member disks:
/// - `1`: legacy disks before `OUTBE_VM_IP` / `OUTBE_RPC_PORT` / `OUTBE_RETH_P2P_PORT`.
/// - `2`: unified `OUTBE_VM_IP` + `OUTBE_RPC_PORT` + `OUTBE_RETH_P2P_PORT`/`OUTBE_RETH_DISCOVERY_PORT`.
pub const CURRENT_ENV_SCHEMA: u32 = 2;
pub const NETWORK_VM_STATE_FILENAME: &str = "network-vm-state.json";
const NETWORK_VM_STATE_LOCK_FILENAME: &str = ".network-vm-state.lock";

const fn legacy_env_schema() -> u32 {
    1
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum MemberRole {
    Founder,
    JoiningValidator,
    FullNode,
}

const fn default_rpc_port() -> u16 {
    super::config::BASE_RPC_PORT
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct NetworkMemberState {
    pub index: usize,
    pub role: MemberRole,
    pub name: String,
    pub vm_name: String,
    pub owner_tag: String,
    pub vmm_host: Ssh,
    pub vmm_home: String,
    pub bridge: String,
    pub service_ip: Ipv4Addr,
    pub advertise_ip: Ipv4Addr,
    #[serde(default = "default_rpc_port")]
    pub rpc_port: u16,
    pub port_offset: u16,
    pub vcpu: u32,
    pub memory_gib: u32,
    pub disk_gib: u32,
    pub flavor: VmFlavor,
    pub no_tee: bool,
    /// Deployed guest image `os_image_hash` for this member.
    pub image_hash: String,
    /// SHA-256 of the prepared raw data disk recorded immediately after `disk prepare`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disk_source_sha256: Option<String>,
    /// Advisory cache of the member's VMM UUID (host `vm status --json` is authoritative).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vm_id: Option<String>,
    /// Advisory cache of the member's MAC address.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mac: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct NetworkVmState {
    pub version: u32,
    pub network_name: String,
    pub chain_id: u64,
    pub genesis_sha256: String,
    pub image_source_id: String,
    pub image_name: String,
    pub image_hash: String,
    pub outbe_up_sha256: String,
    #[serde(default = "legacy_env_schema")]
    pub env_schema: u32,
    #[serde(default)]
    pub members: Vec<NetworkMemberState>,
}

/// Exclusive `O_CLOEXEC` file lock guarding `<artifacts>/network-vm-state.json`.
pub struct NetworkVmStateLock {
    _file: fs::File,
}

impl NetworkVmState {
    pub fn path(artifacts: &Path) -> PathBuf {
        artifacts.join(NETWORK_VM_STATE_FILENAME)
    }

    pub fn lock_path(artifacts: &Path) -> PathBuf {
        artifacts.join(NETWORK_VM_STATE_LOCK_FILENAME)
    }

    /// Acquire an exclusive `O_CLOEXEC` `flock` on `<artifacts>/.network-vm-state.lock`.
    pub fn lock(artifacts: &Path) -> Result<NetworkVmStateLock> {
        ensure!(
            artifacts.is_dir(),
            "network artifacts directory does not exist: {}",
            artifacts.display()
        );
        let lock_path = Self::lock_path(artifacts);
        let current_uid = rustix::process::getuid().as_raw();
        if let Ok(meta) = fs::symlink_metadata(&lock_path) {
            ensure!(
                meta.file_type().is_file(),
                "refusing non-regular network state lock: {}",
                lock_path.display()
            );
            ensure!(
                meta.uid() == current_uid,
                "network state lock {} is owned by uid {}, expected {current_uid}",
                lock_path.display(),
                meta.uid()
            );
        }
        let fd = openat(
            CWD,
            &lock_path,
            OFlags::CREATE | OFlags::RDWR | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::from_raw_mode(0o600),
        )
        .wrap_err_with(|| format!("open network state lock {}", lock_path.display()))?;
        let file = fs::File::from(fd);
        let meta = file
            .metadata()
            .wrap_err_with(|| format!("stat network state lock {}", lock_path.display()))?;
        ensure!(
            meta.file_type().is_file() && meta.uid() == current_uid,
            "refusing unsafe network state lock: {}",
            lock_path.display()
        );
        flock(&file, FlockOperation::LockExclusive)
            .wrap_err_with(|| format!("acquire exclusive lock on {}", lock_path.display()))?;
        Ok(NetworkVmStateLock { _file: file })
    }

    pub fn new(build: &NetworkArtifacts, image_source_id: String, outbe_up_sha256: String) -> Self {
        Self {
            version: NETWORK_VM_STATE_VERSION,
            network_name: build.config.name.clone(),
            chain_id: build.config.chain_id,
            genesis_sha256: build.manifest.genesis.final_sha256.clone(),
            image_source_id,
            image_name: String::new(),
            image_hash: String::new(),
            outbe_up_sha256,
            env_schema: CURRENT_ENV_SCHEMA,
            members: Vec::new(),
        }
    }

    pub fn load_optional_locked(
        _lock: &NetworkVmStateLock,
        artifacts: &Path,
    ) -> Result<Option<Self>> {
        let path = Self::path(artifacts);
        let meta = match fs::symlink_metadata(&path) {
            Ok(meta) => meta,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(err) => {
                return Err(err)
                    .wrap_err_with(|| format!("stat VM network state {}", path.display()))
            }
        };
        ensure!(
            meta.file_type().is_file(),
            "refusing non-regular VM network state file: {}",
            path.display()
        );
        let raw = fs::read(&path)
            .wrap_err_with(|| format!("read VM network state {}", path.display()))?;
        // Check version first so v1 files receive an actionable migration/re-deploy message.
        let version_probe: serde_json::Value = serde_json::from_slice(&raw)
            .wrap_err_with(|| format!("invalid JSON in VM network state: {}", path.display()))?;
        let file_version = version_probe
            .get("version")
            .and_then(serde_json::Value::as_u64)
            .ok_or_else(|| {
                eyre::eyre!(
                    "VM network state {} is missing numeric `version`",
                    path.display()
                )
            })?;
        ensure!(
            file_version == u64::from(NETWORK_VM_STATE_VERSION),
            "unsupported VM network state version {file_version} in {} (expected version {NETWORK_VM_STATE_VERSION}; remove legacy state or run `outbe-net-up network remove` before redeploying)",
            path.display()
        );
        let state: Self = serde_json::from_slice(&raw)
            .wrap_err_with(|| format!("invalid v2 VM network state: {}", path.display()))?;
        Ok(Some(state))
    }

    /// Atomically persist `network-vm-state.json` (`0600`, fsync temp, rename, fsync parent dir).
    pub fn save_locked(&self, _lock: &NetworkVmStateLock, artifacts: &Path) -> Result<PathBuf> {
        ensure!(
            self.version == NETWORK_VM_STATE_VERSION,
            "refusing to save unsupported VM network state version {}",
            self.version
        );
        let path = Self::path(artifacts);
        if let Ok(meta) = fs::symlink_metadata(&path) {
            ensure!(
                meta.file_type().is_file(),
                "refusing to overwrite non-regular VM network state path: {}",
                path.display()
            );
        }
        let mut bytes = serde_json::to_vec_pretty(self)?;
        bytes.push(b'\n');

        let mut tmp = tempfile::Builder::new()
            .prefix(&format!(".{NETWORK_VM_STATE_FILENAME}.tmp."))
            .tempfile_in(artifacts)
            .wrap_err_with(|| {
                format!(
                    "create temporary VM network state file in {}",
                    artifacts.display()
                )
            })?;
        tmp.as_file()
            .set_permissions(fs::Permissions::from_mode(0o600))
            .wrap_err("set 0600 permissions on temporary VM network state file")?;
        tmp.write_all(&bytes)
            .wrap_err("write temporary VM network state file")?;
        tmp.as_file()
            .sync_all()
            .wrap_err("fsync temporary VM network state file")?;
        tmp.persist(&path)
            .map_err(|err| err.error)
            .wrap_err_with(|| format!("commit VM network state {}", path.display()))?;
        if let Ok(dir) = fs::File::open(artifacts) {
            let _ = dir.sync_all();
        }
        Ok(path)
    }

    pub fn delete_locked(_lock: &NetworkVmStateLock, artifacts: &Path) -> Result<bool> {
        let path = Self::path(artifacts);
        match fs::remove_file(&path) {
            Ok(()) => {
                if let Ok(dir) = fs::File::open(artifacts) {
                    let _ = dir.sync_all();
                }
                Ok(true)
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(err) => {
                Err(err).wrap_err_with(|| format!("remove VM network state {}", path.display()))
            }
        }
    }

    pub fn ensure_matches_network(&self, build: &NetworkArtifacts) -> Result<()> {
        ensure!(
            self.network_name == build.config.name
                && self.chain_id == build.config.chain_id
                && self.genesis_sha256 == build.manifest.genesis.final_sha256,
            "network-vm-state.json does not match network artifacts `{}` (chain {})",
            build.config.name,
            build.config.chain_id
        );
        Ok(())
    }

    pub fn member(&self, vm_name: &str) -> Option<&NetworkMemberState> {
        self.members.iter().find(|m| m.vm_name == vm_name)
    }

    pub fn upsert_member(&mut self, entry: NetworkMemberState) {
        if let Some(existing) = self.members.iter_mut().find(|m| m.vm_name == entry.vm_name) {
            *existing = entry;
        } else {
            self.members.push(entry);
            self.members.sort_by_key(|m| (m.index, m.vm_name.clone()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_member(index: usize, name: &str) -> NetworkMemberState {
        NetworkMemberState {
            index,
            role: MemberRole::Founder,
            name: name.to_owned(),
            vm_name: format!("devnet-{name}"),
            owner_tag: format!("outbe-net:devnet:01234567:{name}"),
            vmm_host: Ssh {
                host: "192.0.2.50".to_owned(),
                user: "ubuntu".to_owned(),
                port: 22,
            },
            vmm_home: "~/.dstack-vmm".to_owned(),
            bridge: "obr0".to_owned(),
            service_ip: Ipv4Addr::new(10, 77, 0, 10 + index as u8),
            advertise_ip: Ipv4Addr::new(10, 77, 0, 10 + index as u8),
            rpc_port: default_rpc_port(),
            port_offset: 0,
            vcpu: 3,
            memory_gib: 5,
            disk_gib: 40,
            flavor: VmFlavor::Dev,
            no_tee: false,
            image_hash: "0123456789abcdef".repeat(4),
            disk_source_sha256: Some("a".repeat(64)),
            vm_id: Some("550e8400-e29b-41d4-a716-446655440010".to_owned()),
            mac: Some("52:54:00:aa:bb:cc".to_owned()),
        }
    }

    #[test]
    fn v2_state_round_trips_under_flock_with_0600_and_rejects_v1_and_unknown_fields() {
        let tmp = tempfile::tempdir().unwrap();
        let lock = NetworkVmState::lock(tmp.path()).unwrap();
        assert!(NetworkVmState::load_optional_locked(&lock, tmp.path())
            .unwrap()
            .is_none());

        let state = NetworkVmState {
            version: NETWORK_VM_STATE_VERSION,
            network_name: "devnet".to_owned(),
            chain_id: 424_242,
            genesis_sha256: "0123456789abcdef".repeat(4),
            image_source_id: format!("archive:{}", "1".repeat(64)),
            image_name: "outbe-vm-dev-0123456789abcdef".to_owned(),
            image_hash: "0123456789abcdef".repeat(4),
            outbe_up_sha256: "2".repeat(64),
            env_schema: CURRENT_ENV_SCHEMA,
            members: vec![sample_member(0, "validator-0")],
        };
        let path = state.save_locked(&lock, tmp.path()).unwrap();
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let loaded = NetworkVmState::load_optional_locked(&lock, tmp.path())
            .unwrap()
            .unwrap();
        assert_eq!(loaded, state);
        assert_eq!(loaded.env_schema, CURRENT_ENV_SCHEMA);

        // Legacy v2 state file without `env_schema` defaults to schema 1.
        let mut legacy_v2: serde_json::Value = serde_json::to_value(&state).unwrap();
        legacy_v2.as_object_mut().unwrap().remove("env_schema");
        fs::write(&path, serde_json::to_vec(&legacy_v2).unwrap()).unwrap();
        let legacy_loaded = NetworkVmState::load_optional_locked(&lock, tmp.path())
            .unwrap()
            .unwrap();
        assert_eq!(legacy_loaded.env_schema, 1);

        // Version 1 file is explicitly rejected.
        fs::write(&path, r#"{"version": 1, "founders": []}"#).unwrap();
        let err = NetworkVmState::load_optional_locked(&lock, tmp.path()).unwrap_err();
        assert!(err.to_string().contains("version 1"), "{err}");

        // Unknown fields on version 2 are rejected by deny_unknown_fields.
        let mut raw_v2: serde_json::Value = serde_json::to_value(&state).unwrap();
        raw_v2["unexpected_legacy_phase"] = serde_json::json!("ready");
        fs::write(&path, serde_json::to_vec(&raw_v2).unwrap()).unwrap();
        let err = NetworkVmState::load_optional_locked(&lock, tmp.path()).unwrap_err();
        assert!(format!("{err:#}").contains("unknown field"), "{err:#}");

        assert!(NetworkVmState::delete_locked(&lock, tmp.path()).unwrap());
        assert!(!NetworkVmState::delete_locked(&lock, tmp.path()).unwrap());
    }

    #[test]
    fn flock_serializes_concurrent_state_updates() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().to_path_buf();
        let mut handles = Vec::new();
        for worker in 0..4usize {
            let dir = dir.clone();
            handles.push(std::thread::spawn(move || {
                let lock = NetworkVmState::lock(&dir).unwrap();
                let mut state = NetworkVmState::load_optional_locked(&lock, &dir)
                    .unwrap()
                    .unwrap_or_else(|| NetworkVmState {
                        version: NETWORK_VM_STATE_VERSION,
                        network_name: "devnet".to_owned(),
                        chain_id: 424_242,
                        genesis_sha256: "0123456789abcdef".repeat(4),
                        image_source_id: "archive:abc".to_owned(),
                        image_name: "img".to_owned(),
                        image_hash: "0".repeat(64),
                        outbe_up_sha256: "1".repeat(64),
                        env_schema: CURRENT_ENV_SCHEMA,
                        members: Vec::new(),
                    });
                state.upsert_member(sample_member(worker, &format!("validator-{worker}")));
                state.save_locked(&lock, &dir).unwrap();
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
        let lock = NetworkVmState::lock(&dir).unwrap();
        let final_state = NetworkVmState::load_optional_locked(&lock, &dir)
            .unwrap()
            .unwrap();
        assert_eq!(final_state.members.len(), 4);
    }
}
