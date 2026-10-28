//! Operator-owned Radicle sidecars for release LocalNet scenarios.

use std::fs::{self, OpenOptions};
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::{DirBuilderExt as _, MetadataExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use eyre::{bail, eyre, Result, WrapErr};
use outbe_radicle::integration::query_sidecar;

use crate::internal::eth::block_on;
use crate::internal::proc::{args, ChildGuard};

use super::Localnet;

mod governments;
mod readiness;
mod source;

pub(super) use readiness::provisioned_radicle_node_id;
use readiness::{native_address, native_node_id, wait_radicle_ready, RadicleReadiness};
use source::{
    configure_user_radicle, connected_sessions, native_sessions, repository_visible,
    require_public_source_repository, seed_scope_all,
};

const VALIDATOR_SET_ALLOC_ADDRESS: &str = "000000000000000000000000000000000000ee00";
const VALIDATOR_SET_MAX_VALIDATORS_SLOT: &str =
    "0x0000000000000000000000000000000000000000000000000000000000000001";
const PORTABLE_UNIX_SOCKET_PATH_LIMIT: usize = 104;
const NATIVE_CONTROL_DEADLINE: Duration = Duration::from_secs(5);
const RADICLE_READY_TIMEOUT: Duration = Duration::from_secs(10);
const RADICLE_READY_POLL: Duration = Duration::from_millis(100);
const RADICLE_SOURCE_DISCOVERY_TIMEOUT: Duration = Duration::from_secs(180);

#[derive(Clone, Debug)]
pub struct RadicleRepositoryFixtureV1 {
    pub home: PathBuf,
    pub worktree: PathBuf,
    pub repo_id: String,
    pub repo_id_hex: String,
    pub issue_id: String,
    pub patch_id: String,
    pub pushed_commit: Option<String>,
}

impl Localnet {
    pub(crate) fn radicle_control_socket(&self, index: usize) -> std::path::PathBuf {
        self.cfg.radicle_control_socket(index)
    }

    /// Start the operator-owned foreground sidecar before its validator node.
    pub(crate) fn start_radicle(&mut self, index: usize) -> Result<()> {
        self.start_radicle_at(index, self.cfg.radicle_port(index))
    }

    fn start_radicle_at(&mut self, index: usize, peer_port: u16) -> Result<()> {
        if let Some(guard) = self.radicle_sidecars.get_mut(&index) {
            if guard.exit_status()?.is_none() {
                return Ok(());
            }
        }
        self.radicle_sidecars.remove(&index);

        let validator_dir = self.cfg.validator_dir(index);
        let home = validator_dir.join("radicle");
        let key = home.join("keys/radicle");
        if !key.is_file() {
            bail!(
                "validator-{index} has no Radicle key at {}; bootstrap or install the identity first",
                key.display()
            );
        }
        let node_id = provisioned_radicle_node_id(&home)?;
        fs::create_dir_all(validator_dir.join("logs"))?;

        let script = self.cfg.repo.join("deploy/validator/run-radicle.sh");
        let listen = format!("127.0.0.1:{peer_port}");
        let status = format!("127.0.0.1:{}", self.cfg.radicle_status_port(index));
        let advertise = listen.clone();
        let max_validators = materialized_max_validators(&self.cfg.dir.join("genesis.json"))?;
        let control_socket = self.radicle_control_socket(index);
        prepare_private_socket_parent(
            &self.cfg.radicle_runtime_root,
            &control_socket,
            &validator_dir,
        )?;
        let mut command = Command::new(&script);
        command
            .env("OUTBE_RADICLE_BINARY", &self.cfg.bin_radicle)
            .env("OUTBE_RADICLE_CONTROL_SOCKET", &control_socket)
            .args(args![
                home.display(),
                listen,
                status,
                max_validators,
                advertise,
            ]);
        let log_path = validator_dir.join("radicle.log");
        let log = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)?;
        command
            .stdout(Stdio::from(log.try_clone()?))
            .stderr(Stdio::from(log))
            .stdin(Stdio::null());

        let label = format!("radicle-{index}");
        let mut guard = ChildGuard::spawn(&label, command)
            .wrap_err_with(|| format!("start {label} through {}", script.display()))?;
        wait_radicle_ready(
            &mut guard,
            &RadicleReadiness {
                expected_node_id: node_id,
                control_socket: &control_socket,
                peer: ([127, 0, 0, 1], peer_port).into(),
                status: ([127, 0, 0, 1], self.cfg.radicle_status_port(index)).into(),
                log: &log_path,
            },
            RADICLE_READY_TIMEOUT,
        )?;
        self.radicle_sidecars.insert(index, guard);
        Ok(())
    }

    /// Stop only the selected sidecar; consensus ownership remains with the node.
    pub fn stop_radicle(&mut self, index: usize) -> Result<()> {
        self.radicle_sidecars
            .remove(&index)
            .ok_or_else(|| eyre!("radicle-{index} is not running"))?;
        Ok(())
    }

    /// Restart one sidecar with the same key and persistent home.
    pub fn restart_radicle(&mut self, index: usize) -> Result<()> {
        self.radicle_sidecars.remove(&index);
        self.start_radicle(index)
    }

    /// Restart one sidecar on a new peer port without restarting outbe-chain.
    pub fn restart_radicle_at_port(&mut self, index: usize, peer_port: u16) -> Result<()> {
        self.radicle_sidecars.remove(&index);
        self.start_radicle_at(index, peer_port)
    }

    /// Use a disjoint harness port block for endpoint-replacement evidence.
    pub fn restart_radicle_on_alternate_port(&mut self, index: usize) -> Result<u16> {
        let peer_port = self.cfg.radicle_port(index + 64);
        self.restart_radicle_at_port(index, peer_port)?;
        Ok(peer_port)
    }

    pub fn radicle_pid(&self, index: usize) -> Result<u32> {
        self.radicle_sidecars
            .get(&index)
            .map(ChildGuard::pid)
            .ok_or_else(|| eyre!("radicle-{index} is not running"))
    }

    pub fn validator_radicle_addresses(&self, index: usize) -> Result<Vec<String>> {
        let socket = self.radicle_control_socket(index);
        let info = block_on(query_sidecar(socket, NATIVE_CONTROL_DEADLINE))
            .wrap_err_with(|| format!("query validator-{index} native Radicle identity/config"))?;
        info.addresses
            .iter()
            .map(|address| {
                Ok(format!(
                    "{}@{}",
                    native_node_id(info.node_id),
                    native_address(address)?
                ))
            })
            .collect()
    }

    fn prepare_user_radicle_home(&self, name: &str) -> Result<PathBuf> {
        let revision = self.git(&self.cfg.repo, &["rev-parse", "HEAD"])?;
        for (name, binary) in [
            ("rad", &self.cfg.bin_rad),
            ("git-remote-rad", &self.cfg.bin_git_remote_rad),
        ] {
            let mut command = Command::new(binary);
            command.arg("--version");
            verify_user_tool_version(name, &output(command, name)?, revision.trim())
                .wrap_err_with(|| format!("verify user Radicle tool {}", binary.display()))?;
        }
        let home = self.cfg.dir.join(name);
        if !home.exists() {
            let output = Command::new(&self.cfg.bin_keygen)
                .args(["radicle", "--output-dir"])
                .arg(&home)
                .output()
                .wrap_err("generate independent user Radicle key")?;
            if !output.status.success() {
                bail!(
                    "generate independent user Radicle key failed: {}",
                    String::from_utf8_lossy(&output.stderr)
                );
            }
        }
        for dir in ["storage", "node", "cobs"] {
            let path = home.join(dir);
            fs::create_dir_all(&path)?;
            fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
        }
        write_outbe_profile(
            &home,
            &format!(
                "127.0.0.1:{}",
                self.cfg.radicle_port(self.committee_size() + 32)
            ),
            materialized_max_validators(&self.cfg.dir.join("genesis.json"))?,
        )?;

        Ok(home)
    }

    /// Create a public source repository while its non-validator sidecar is offline.
    pub fn prepare_user_radicle_repository(&self) -> Result<RadicleRepositoryFixtureV1> {
        let home = self.prepare_user_radicle_home("user-radicle")?;

        let worktree = self.cfg.dir.join("user-repository");
        fs::create_dir_all(&worktree)?;
        self.git(&worktree, &["init", "--initial-branch", "master"])?;
        self.git(&worktree, &["config", "user.name", "Outbe E2E"])?;
        self.git(
            &worktree,
            &["config", "user.email", "radicle-e2e@outbe.test"],
        )?;
        fs::write(worktree.join("README.md"), "# Outbe Radicle E2E\n")?;
        self.git(&worktree, &["add", "README.md"])?;
        self.git(&worktree, &["commit", "-m", "Initial source"])?;
        self.rad(
            &home,
            &self.cfg.user_radicle_control_socket(),
            Some(&worktree),
            &[
                "init",
                "--name",
                "outbe-radicle-e2e",
                "--description",
                "Outbe validator replication fixture",
                "--no-confirm",
                "--public",
                "--no-seed",
            ],
        )?;
        let mut initializer = self.spawn_user_radicle(&home)?;
        initializer.stop();
        self.rad(
            &home,
            &self.cfg.user_radicle_control_socket(),
            Some(&worktree),
            &["cob", "migrate"],
        )?;

        let remote = self.git(&worktree, &["config", "--get", "remote.rad.url"])?;
        let repo_id = remote
            .trim()
            .strip_prefix("rad://")
            .and_then(|remote| remote.split('/').next())
            .map(str::to_owned)
            .ok_or_else(|| eyre!("rad init installed an invalid remote URL: {remote}"))?;
        let repo_id_hex = hex::encode(decode_repo_id(&repo_id)?);
        let repo_id = format!("rad:{repo_id}");
        let issue = self.rad(
            &home,
            &self.cfg.user_radicle_control_socket(),
            Some(&worktree),
            &[
                "issue",
                "open",
                "--title",
                "Validator replication evidence",
                "--description",
                "Created before the source sidecar starts",
                "--no-announce",
            ],
        )?;
        let issue_id = first_hex40(&issue).ok_or_else(|| eyre!("rad issue returned no ID"))?;

        self.git(&worktree, &["checkout", "-b", "radicle-e2e-patch"])?;
        fs::write(
            worktree.join("PATCH-EVIDENCE.md"),
            "This change must replicate through validator storage.\n",
        )?;
        self.git(&worktree, &["add", "PATCH-EVIDENCE.md"])?;
        self.git(&worktree, &["commit", "-m", "Add replication evidence"])?;
        let patch = self.git_env(
            &home,
            &self.cfg.user_radicle_control_socket(),
            &worktree,
            &[
                "push",
                "-o",
                "patch.message=Outbe replication patch",
                "rad",
                "HEAD:refs/patches",
            ],
        )?;
        let patch_id = first_hex40(&patch).ok_or_else(|| eyre!("git push returned no patch ID"))?;

        Ok(RadicleRepositoryFixtureV1 {
            home,
            worktree,
            repo_id,
            repo_id_hex,
            issue_id,
            patch_id,
            pushed_commit: None,
        })
    }

    /// Start the independent source and explicitly connect it to every validator.
    pub fn start_user_radicle(&mut self, fixture: &RadicleRepositoryFixtureV1) -> Result<()> {
        require_public_source_repository(fixture)?;
        if let Some(guard) = self.user_radicle.as_mut() {
            if guard.exit_status()?.is_none() {
                return Ok(());
            }
        }
        self.user_radicle = None;
        // Retain local ownership until all setup succeeds. Validators' policies
        // remain manager-owned. Only this independent source is explicitly seeded.
        let guard = self.spawn_user_radicle(&fixture.home)?;
        configure_user_radicle(
            self.cfg.user_radicle_control_socket(),
            decode_repo_id(&fixture.repo_id)?,
            (0..self.committee_size())
                .map(|index| self.radicle_control_socket(index))
                .collect(),
        )?;
        self.user_radicle = Some(guard);
        Ok(())
    }

    fn spawn_user_radicle(&self, home: &Path) -> Result<ChildGuard> {
        let node_id = provisioned_radicle_node_id(home)?;
        let slot = self.committee_size() + 32;
        let listen = format!("127.0.0.1:{}", self.cfg.radicle_port(slot));
        let status = format!("127.0.0.1:{}", self.cfg.radicle_status_port(slot));
        let script = self.cfg.repo.join("deploy/validator/run-radicle.sh");
        let max_validators = materialized_max_validators(&self.cfg.dir.join("genesis.json"))?;
        let control_socket = self.cfg.user_radicle_control_socket();
        prepare_private_socket_parent(&self.cfg.radicle_runtime_root, &control_socket, home)?;
        let mut command = Command::new(&script);
        command
            .env("OUTBE_RADICLE_BINARY", &self.cfg.bin_radicle)
            .env("OUTBE_RADICLE_CONTROL_SOCKET", &control_socket)
            .args(args![
                home.display(),
                listen,
                status,
                max_validators,
                format!("127.0.0.1:{}", self.cfg.radicle_port(slot)),
            ]);
        let log_path = self.cfg.dir.join("user-radicle.log");
        let log = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)?;
        command
            .stdout(Stdio::from(log.try_clone()?))
            .stderr(Stdio::from(log))
            .stdin(Stdio::null());
        let mut guard = ChildGuard::spawn("radicle-user", command)?;
        wait_radicle_ready(
            &mut guard,
            &RadicleReadiness {
                expected_node_id: node_id,
                control_socket: &control_socket,
                peer: ([127, 0, 0, 1], self.cfg.radicle_port(slot)).into(),
                status: ([127, 0, 0, 1], self.cfg.radicle_status_port(slot)).into(),
                log: &log_path,
            },
            RADICLE_READY_TIMEOUT,
        )?;
        Ok(guard)
    }

    /// Publish one ordinary Git update after the source node is online.
    pub fn push_user_radicle_update(&self, fixture: &mut RadicleRepositoryFixtureV1) -> Result<()> {
        self.push_user_radicle_commit(
            fixture,
            "# Outbe Radicle E2E\n\nReplicated through the validator mesh.\n",
            "Publish validator mesh evidence",
        )
    }

    pub fn push_user_radicle_recovery_update(
        &self,
        fixture: &mut RadicleRepositoryFixtureV1,
    ) -> Result<()> {
        self.push_user_radicle_commit(
            fixture,
            "# Outbe Radicle E2E\n\nNew Git head during validator sidecar outage.\n",
            "Publish head during sidecar outage",
        )
    }

    fn push_user_radicle_commit(
        &self,
        fixture: &mut RadicleRepositoryFixtureV1,
        readme: &str,
        message: &str,
    ) -> Result<()> {
        fs::write(fixture.worktree.join("README.md"), readme)?;
        self.git(&fixture.worktree, &["add", "README.md"])?;
        self.git(&fixture.worktree, &["commit", "-m", message])?;
        self.git_env(
            &fixture.home,
            &self.cfg.user_radicle_control_socket(),
            &fixture.worktree,
            &["push", "rad", "HEAD:refs/heads/master"],
        )?;
        fixture.pushed_commit = Some(
            self.git(&fixture.worktree, &["rev-parse", "HEAD"])?
                .trim()
                .to_owned(),
        );
        Ok(())
    }

    pub fn radicle_repo_visible(
        &self,
        validator: usize,
        fixture: &RadicleRepositoryFixtureV1,
    ) -> Result<bool> {
        let home = self.cfg.validator_dir(validator).join("radicle");
        repository_visible(&home, fixture)
            .wrap_err_with(|| format!("observe validator-{validator} replicated repository"))
    }

    pub fn radicle_node_status(&self, index: usize) -> Result<String> {
        let sessions = native_sessions(self.radicle_control_socket(index))?;
        let sessions = sessions
            .iter()
            .map(|session| {
                Ok(serde_json::json!({
                    "nodeId": native_node_id(session.node_id),
                    "address": native_address(&session.address)?,
                    "connected": session.connected,
                }))
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(serde_json::to_string(&sessions)?)
    }

    /// Exact connected native Heartwood peers from typed UDS control responses.
    pub fn radicle_connected_session_node_ids(&self, index: usize) -> Result<Vec<String>> {
        Ok(
            connected_sessions(native_sessions(self.radicle_control_socket(index))?)?
                .into_iter()
                .map(|(node_id, _)| node_id)
                .collect(),
        )
    }

    /// Connected native Heartwood address for one exact peer NodeId.
    pub fn radicle_connected_session_address(&self, index: usize, node_id: &str) -> Result<String> {
        connected_sessions(native_sessions(self.radicle_control_socket(index))?)?
            .into_iter()
            .find_map(|(observed, address)| (observed == node_id).then_some(address))
            .ok_or_else(|| eyre!("validator-{index} has no connected session for {node_id}"))
    }

    /// Native persistent policy proof that the manager applied Seed Scope::All.
    pub fn radicle_seed_scope_all(&self, index: usize, repo_id: &str) -> Result<bool> {
        let home = self.cfg.validator_dir(index).join("radicle");
        seed_scope_all(&home, repo_id)
            .wrap_err_with(|| format!("observe validator-{index} persistent seeding policy"))
    }

    fn rad(
        &self,
        home: &Path,
        control_socket: &Path,
        cwd: Option<&Path>,
        args: &[&str],
    ) -> Result<String> {
        let mut command = Command::new(&self.cfg.bin_rad);
        command
            .env("RAD_HOME", home)
            .env("RAD_SOCKET", control_socket)
            .env("PATH", self.radicle_path())
            .args(args);
        if let Some(cwd) = cwd {
            command.current_dir(cwd);
        }
        output(command, "rad")
    }

    fn git(&self, cwd: &Path, args: &[&str]) -> Result<String> {
        let mut command = Command::new("git");
        command.current_dir(cwd).args(args);
        output(command, "git")
    }

    fn git_env(
        &self,
        home: &Path,
        control_socket: &Path,
        cwd: &Path,
        args: &[&str],
    ) -> Result<String> {
        let mut command = Command::new("git");
        command
            .current_dir(cwd)
            .env("RAD_HOME", home)
            .env("RAD_SOCKET", control_socket)
            .env("PATH", self.radicle_path())
            .args(args);
        output(command, "git")
    }

    fn radicle_path(&self) -> std::ffi::OsString {
        let helper_dir = self
            .cfg
            .bin_git_remote_rad
            .parent()
            .unwrap_or_else(|| Path::new("."));
        let mut paths = vec![helper_dir.to_path_buf()];
        paths.extend(std::env::split_paths(&self.cfg.path));
        std::env::join_paths(paths).unwrap_or_else(|_| self.cfg.path.clone().into())
    }

    pub(crate) fn cleanup_radicle_runtime(&self) -> Result<()> {
        let target = if self.cfg.scenario == 0 {
            self.cfg.radicle_runtime_root.clone()
        } else {
            self.cfg.radicle_scenario_runtime_dir()
        };
        if !target.starts_with(&self.cfg.radicle_runtime_root) {
            bail!("refusing to clean Radicle runtime path outside its run root");
        }
        let metadata = match fs::symlink_metadata(&target) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error).wrap_err("inspect Radicle runtime directory"),
        };
        let expected_uid = fs::symlink_metadata(&self.cfg.dir)
            .wrap_err_with(|| format!("inspect localnet directory {}", self.cfg.dir.display()))?
            .uid();
        validate_private_owned_dir(&target, &metadata, expected_uid)?;
        fs::remove_dir_all(&target)
            .wrap_err_with(|| format!("remove Radicle runtime directory {}", target.display()))
    }
}

fn prepare_private_socket_parent(
    runtime_root: &Path,
    socket: &Path,
    owner_anchor: &Path,
) -> Result<()> {
    if socket.as_os_str().as_bytes().len() >= PORTABLE_UNIX_SOCKET_PATH_LIMIT {
        bail!(
            "Radicle control socket path must be shorter than {PORTABLE_UNIX_SOCKET_PATH_LIMIT} bytes: {}",
            socket.display()
        );
    }
    let parent = socket
        .parent()
        .ok_or_else(|| eyre!("Radicle control socket has no parent"))?;
    if !parent.starts_with(runtime_root) {
        bail!("Radicle control socket escaped its run-scoped runtime root");
    }
    let expected_uid = fs::symlink_metadata(owner_anchor)
        .wrap_err_with(|| format!("inspect Radicle owner anchor {}", owner_anchor.display()))?
        .uid();
    ensure_private_owned_dir(runtime_root, expected_uid)?;
    ensure_private_owned_dir(parent, expected_uid)
}

fn ensure_private_owned_dir(path: &Path, expected_uid: u32) -> Result<()> {
    let mut builder = fs::DirBuilder::new();
    builder.mode(0o700);
    match builder.create(path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => {
            return Err(error)
                .wrap_err_with(|| format!("create Radicle runtime directory {}", path.display()));
        }
    }
    let metadata = fs::symlink_metadata(path)
        .wrap_err_with(|| format!("inspect Radicle runtime directory {}", path.display()))?;
    validate_private_owned_dir(path, &metadata, expected_uid)
}

fn validate_private_owned_dir(
    path: &Path,
    metadata: &fs::Metadata,
    expected_uid: u32,
) -> Result<()> {
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        bail!(
            "Radicle runtime path must be a non-symlink directory: {}",
            path.display()
        );
    }
    let mode = metadata.mode() & 0o777;
    if metadata.uid() != expected_uid || mode != 0o700 {
        bail!(
            "Radicle runtime directory {} must be owned by uid {expected_uid} with mode 700 (found uid {}, mode {mode:o})",
            path.display(),
            metadata.uid()
        );
    }
    Ok(())
}

fn output(mut command: Command, label: &str) -> Result<String> {
    let result = command.output().wrap_err_with(|| format!("run {label}"))?;
    let stdout = String::from_utf8_lossy(&result.stdout);
    let stderr = String::from_utf8_lossy(&result.stderr);
    if !result.status.success() {
        bail!("{label} failed: {stderr}{stdout}");
    }
    Ok(format!("{stdout}{stderr}"))
}

fn first_hex40(value: &str) -> Option<String> {
    value
        .split(|character: char| !character.is_ascii_hexdigit())
        .find(|token| token.len() == 40)
        .map(str::to_owned)
}

fn decode_repo_id(value: &str) -> Result<[u8; 20]> {
    let canonical = value.strip_prefix("rad:").unwrap_or(value);
    let (base, bytes) = multibase::decode(canonical).wrap_err("decode canonical Radicle RepoId")?;
    if base != multibase::Base::Base58Btc {
        bail!("Radicle RepoId must use base58btc multibase encoding");
    }
    bytes
        .try_into()
        .map_err(|bytes: Vec<u8>| eyre!("Radicle RepoId must be 20 bytes, got {}", bytes.len()))
}

fn verify_user_tool_version(name: &str, version: &str, revision: &str) -> Result<()> {
    let version = version.trim();
    let commit = version
        .strip_prefix(&format!("{name} "))
        .and_then(|value| value.rsplit_once(" ("))
        .and_then(|(_, commit)| commit.strip_suffix(')'))
        .filter(|commit| commit.len() >= 7 && commit.bytes().all(|byte| byte.is_ascii_hexdigit()));
    if !commit.is_some_and(|commit| revision.starts_with(commit)) {
        bail!("{name} must come from vendored Heartwood at chain revision {revision}; observed {version:?}");
    }
    Ok(())
}

fn write_outbe_profile(home: &Path, listen: &str, max_validators: usize) -> Result<()> {
    let managed_peers = max_validators.saturating_sub(1);
    let profile = serde_json::json!({
        "preferredSeeds": [],
        "node": {
            "alias": "outbe",
            "listen": [listen],
            "peers": { "type": "static" },
            "connect": [],
            "externalAddresses": [listen],
            "network": "outbe",
            "relay": "always",
            "limits": {
                "connection": {
                    "inbound": managed_peers + 16,
                    "outbound": managed_peers,
                }
            },
            "seedingPolicy": { "default": "block" },
        }
    });
    let path = home.join("config.json");
    fs::write(&path, serde_json::to_vec_pretty(&profile)?)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    Ok(())
}

fn materialized_max_validators(genesis_path: &Path) -> Result<usize> {
    let genesis: serde_json::Value = serde_json::from_slice(
        &fs::read(genesis_path)
            .wrap_err_with(|| format!("read materialized genesis {}", genesis_path.display()))?,
    )
    .wrap_err_with(|| format!("decode materialized genesis {}", genesis_path.display()))?;
    let alloc = genesis
        .get("alloc")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| eyre!("materialized genesis has no alloc object"))?;
    let account = alloc
        .get(VALIDATOR_SET_ALLOC_ADDRESS)
        .or_else(|| alloc.get(&format!("0x{VALIDATOR_SET_ALLOC_ADDRESS}")))
        .ok_or_else(|| eyre!("materialized genesis has no ValidatorSet alloc"))?;
    let storage = account
        .get("storage")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| eyre!("materialized ValidatorSet alloc has no storage"))?;
    let encoded = storage
        .get(VALIDATOR_SET_MAX_VALIDATORS_SLOT)
        .or_else(|| storage.get(VALIDATOR_SET_MAX_VALIDATORS_SLOT.trim_start_matches("0x")))
        .ok_or_else(|| eyre!("materialized ValidatorSet has no maxValidators slot"))?;
    let max_validators = encoded
        .as_u64()
        .or_else(|| {
            encoded
                .as_str()
                .and_then(|raw| u64::from_str_radix(raw.trim_start_matches("0x"), 16).ok())
        })
        .and_then(|value| usize::try_from(value).ok())
        .filter(|value| *value > 0)
        .ok_or_else(|| eyre!("materialized ValidatorSet maxValidators is not positive"))?;
    Ok(max_validators)
}

#[cfg(test)]
mod tests;
