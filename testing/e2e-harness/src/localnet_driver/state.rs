//! Durable LocalNet owner state, exact process identity and owned path validation.
use super::*;

pub(super) fn read_bootstrap(cli: &LocalnetCli) -> Result<LocalnetBootstrapV1> {
    let path = bootstrap_path(&cli.data_dir);
    let receipt: LocalnetBootstrapV1 = read_json(&path)
        .wrap_err_with(|| format!("LocalNet is not bootstrapped at {}", cli.data_dir.display()))?;
    ensure!(
        receipt.version == STATE_VERSION,
        "unsupported bootstrap version"
    );
    ensure!(
        receipt.lane == "dev_mock",
        "bootstrap is not the dev/mock lane"
    );
    ensure!(receipt.repo == cli.repo, "bootstrap repository differs");
    ensure!(
        receipt.data_dir == cli.data_dir,
        "bootstrap data directory differs"
    );
    ensure!(
        receipt.validators == cli.validators,
        "bootstrap validator count differs"
    );
    let env = cli.environment(Some(&receipt.port_blocks))?;
    let config = Config::resolve(&env);
    ensure!(
        rpc_ports(&config) == receipt.rpc_ports,
        "bootstrap RPC projection differs from persisted service-port layout"
    );
    Ok(receipt)
}

pub(super) fn ensure_not_running(data_dir: &Path) -> Result<()> {
    if let Some(state) = read_state(data_dir)? {
        ensure!(
            !state_is_live(&state),
            "LocalNet is already running (pid {})",
            state.owner_pid
        );
        remove_state(data_dir)?;
    }
    Ok(())
}

pub(super) fn ensure_matches(cli: &LocalnetCli, state: &LocalnetStateV1) -> Result<()> {
    ensure!(state.version == STATE_VERSION, "unsupported state version");
    ensure!(state.lane == "dev_mock", "state is not the dev/mock lane");
    ensure!(state.repo == cli.repo, "state repository differs");
    ensure!(
        state.data_dir == cli.data_dir,
        "state data directory differs"
    );
    ensure!(
        state.validators == cli.validators,
        "state validator count differs"
    );
    Ok(())
}

/// The genesis-funded accounts an operator drives this localnet with.
///
/// A development network is unusable without them and they otherwise exist only
/// on disk, so `start` prints them. These are throwaway devnet keys generated
/// into the data directory for a chain that is wiped on the next bootstrap.
/// Nothing outside a localnet may print key material this way.
pub(super) fn print_funded_accounts(cli: &LocalnetCli, rpc_ports: &[u16]) {
    let rpc = rpc_ports
        .first()
        .map(|port| format!("http://127.0.0.1:{port}"));
    println!(
        "Funded genesis accounts (also at {}/validator-<i>/evm-key.hex):",
        cli.data_dir.display()
    );
    for index in 0..cli.validators {
        let Ok(key) = proc::read_evm_key(&cli.data_dir.join(format!("validator-{index}"))) else {
            println!("  validator-{index}: <evm-key.hex unreadable>");
            continue;
        };
        let Some(address) = eth::address_of(&key) else {
            println!("  validator-{index}: <evm-key.hex is not a valid private key>");
            continue;
        };
        let balance = rpc
            .as_deref()
            .and_then(|url| eth::balance(url, address))
            .map_or_else(|| "unknown".to_owned(), coen);
        println!("  validator-{index}  {address}  {key}  {balance} COEN");
    }
    if let Some(rpc) = rpc {
        println!("Primary RPC: {rpc}");
    }
}

/// COEN carries 18 decimals. Render base units as a decimal amount.
pub(super) fn coen(balance: alloy_primitives::U256) -> String {
    const UNITS: u64 = 1_000_000_000_000_000_000;
    let whole = balance / alloy_primitives::U256::from(UNITS);
    let fraction = balance % alloy_primitives::U256::from(UNITS);
    format!("{whole}.{:018}", fraction.to::<u64>())
}

/// One line naming the resolved enclave profile, so no run is ambiguous about
/// which lane produced it.
pub(super) fn enclave_profile_banner() -> String {
    let mode = localnet_tee_mode();
    let detail = if mode.runs_native_host_enclave() {
        "mock enclave as a host process; no Gramine, no LibOS, no attestation"
    } else {
        "mock enclave under gramine-direct in the pinned test container"
    };
    format!("{} - {detail}", mode.evidence_name())
}

/// The enclave execution profile this host can actually run.
///
/// The Gramine test image is published for `linux/amd64` only and does not
/// survive emulation, so every non-Linux host runs the mock enclave as a native
/// host process instead. This is a distinct named profile with its own evidence
/// label. It is never a silent fallback to `mock`'s.
pub(super) const fn localnet_tee_mode() -> TeeMode {
    if cfg!(target_os = "linux") {
        TeeMode::Mock
    } else {
        TeeMode::MockNative
    }
}

pub(super) fn state_is_live(state: &LocalnetStateV1) -> bool {
    process_identity(state.owner_pid).as_deref() == Some(&state.owner_process_identity)
}

pub(super) fn process_identity(pid: u32) -> Option<String> {
    #[cfg(target_os = "linux")]
    {
        let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        let after_name = stat.rsplit_once(')')?.1.trim();
        let start_ticks = after_name.split_whitespace().nth(19)?;
        let executable = fs::read_link(format!("/proc/{pid}/exe")).ok()?;
        Some(format!("linux:{start_ticks}:{}", executable.display()))
    }
    #[cfg(not(target_os = "linux"))]
    {
        let output = Command::new("ps")
            .args(["-p", &pid.to_string(), "-o", "lstart=", "-o", "command="])
            .output()
            .ok()?;
        output
            .status
            .success()
            .then(|| format!("ps:{}", String::from_utf8_lossy(&output.stdout).trim()))
    }
}

pub(super) fn rpc_ports(config: &Config) -> Vec<u16> {
    (0..config.validators)
        .map(|index| config.http_port(index))
        .collect()
}

pub(super) fn read_state(data_dir: &Path) -> Result<Option<LocalnetStateV1>> {
    let path = state_path(data_dir);
    if !path.exists() {
        return Ok(None);
    }
    read_json(&path).map(Some)
}

pub(super) fn remove_state(data_dir: &Path) -> Result<()> {
    match fs::remove_file(state_path(data_dir)) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

pub(super) fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T> {
    serde_json::from_slice(&fs::read(path)?).map_err(Into::into)
}

pub(super) fn write_json_atomic<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| eyre!("state path has no parent"))?;
    fs::create_dir_all(parent)?;
    let temporary = parent.join(format!(
        ".{}.tmp",
        path.file_name().unwrap().to_string_lossy()
    ));
    let mut file = File::create(&temporary)?;
    file.write_all(&serde_json::to_vec_pretty(value)?)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    fs::rename(&temporary, path)?;
    Ok(())
}

pub(super) fn state_path(data_dir: &Path) -> PathBuf {
    data_dir.join(STATE_FILE)
}

pub(super) fn bootstrap_path(data_dir: &Path) -> PathBuf {
    data_dir.join(BOOTSTRAP_FILE)
}

pub(super) fn resolve_existing_path(path: &Path) -> Result<PathBuf> {
    let absolute = lexical_absolute(path)?;
    fs::canonicalize(&absolute)
        .wrap_err_with(|| format!("resolve existing path {}", absolute.display()))
}

pub(super) fn resolve_maybe_missing_path(path: &Path) -> Result<PathBuf> {
    let normalized = lexical_absolute(path)?;
    let mut existing = normalized.as_path();
    let mut tail = Vec::new();
    while !existing.exists() {
        let name = existing
            .file_name()
            .ok_or_else(|| eyre!("path has no existing ancestor: {}", normalized.display()))?;
        tail.push(name.to_os_string());
        existing = existing
            .parent()
            .ok_or_else(|| eyre!("path has no parent: {}", normalized.display()))?;
    }
    let mut resolved = fs::canonicalize(existing)?;
    for component in tail.iter().rev() {
        resolved.push(component);
    }
    Ok(resolved)
}

pub(super) fn lexical_absolute(path: &Path) -> Result<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let mut normalized = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            Component::RootDir => normalized.push(Path::new("/")),
            Component::CurDir => {}
            Component::ParentDir => {
                ensure!(
                    normalized.pop(),
                    "path escapes the filesystem root: {}",
                    absolute.display()
                );
            }
            Component::Normal(part) => normalized.push(part),
        }
    }
    Ok(normalized)
}

pub(super) fn validate_data_dir(repo: &Path, data_dir: &Path) -> Result<()> {
    ensure!(
        data_dir.is_absolute(),
        "LocalNet data directory must be absolute"
    );
    ensure!(
        data_dir != Path::new("/"),
        "refuse root as LocalNet data directory"
    );
    ensure!(
        data_dir != repo,
        "refuse repository root as LocalNet data directory"
    );
    ensure!(
        !repo.starts_with(data_dir),
        "refuse a LocalNet data directory that owns the repository"
    );
    ensure!(
        data_dir.file_name().is_some(),
        "LocalNet data directory has no name"
    );
    Ok(())
}

pub(super) struct StartLock {
    pub(super) path: PathBuf,
    record: StartLockRecordV1,
}

impl StartLock {
    pub(super) fn acquire(data_dir: &Path) -> Result<Self> {
        fs::create_dir_all(data_dir)?;
        let path = data_dir.join(START_LOCK_FILE);
        let pid = std::process::id();
        let record = StartLockRecordV1 {
            pid,
            process_identity: process_identity(pid)
                .ok_or_else(|| eyre!("cannot establish localnet-start process identity"))?,
        };
        for _ in 0..2 {
            match OpenOptions::new().create_new(true).write(true).open(&path) {
                Ok(mut file) => {
                    file.write_all(&serde_json::to_vec(&record)?)?;
                    file.write_all(b"\n")?;
                    file.sync_all()?;
                    return Ok(Self { path, record });
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    let existing: StartLockRecordV1 =
                        read_json(&path).wrap_err("read existing LocalNet start lock")?;
                    if process_identity(existing.pid).as_deref() == Some(&existing.process_identity)
                    {
                        bail!(
                            "LocalNet start is already in progress (pid {})",
                            existing.pid
                        );
                    }
                    fs::remove_file(&path).wrap_err("remove stale LocalNet start lock")?;
                }
                Err(error) => return Err(error.into()),
            }
        }
        bail!("could not acquire LocalNet start lock");
    }
}

impl Drop for StartLock {
    fn drop(&mut self) {
        let still_ours =
            read_json::<StartLockRecordV1>(&self.path).is_ok_and(|record| record == self.record);
        if still_ours {
            let _ = fs::remove_file(&self.path);
        }
    }
}

#[cfg(feature = "ocomp-integration")]
pub(super) struct StateGuard(pub(super) PathBuf);

#[cfg(feature = "ocomp-integration")]
impl Drop for StateGuard {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}
