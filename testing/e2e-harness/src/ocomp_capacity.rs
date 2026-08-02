//! Linux resource accounting for OCOMP cold capacity runs.
//!
//! The meter reads kernel-owned cgroup-v2 and network counters before and
//! after one run, then measures the exact regular-file bytes retained in its
//! CAS roots. It does not infer resource use from logs or source text.

use std::fs;
#[cfg(feature = "ocomp-integration")]
use std::fs::{File, OpenOptions};
#[cfg(feature = "ocomp-integration")]
use std::io::Write;
use std::path::{Path, PathBuf};
#[cfg(feature = "ocomp-integration")]
use std::time::Instant;

use eyre::{bail, ensure, Result, WrapErr};
use serde::{Deserialize, Serialize};
use walkdir::WalkDir;

#[cfg(feature = "ocomp-integration")]
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct OcompCapacityHostObservationV1 {
    pub architecture: String,
    pub operating_system: String,
    pub logical_cpu_count: u64,
    pub physical_memory_bytes: u64,
    pub root_disk_bytes: u64,
    pub free_workspace_bytes: u64,
    pub block_iops: u64,
    pub block_throughput_bytes_per_second: u64,
    pub pid1_is_systemd: bool,
    pub unified_cgroup_v2: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct OcompCapacityResourceObservationV1 {
    pub cgroup_path: String,
    pub resource_cgroup_writable: bool,
    pub memory_limit_bytes: u64,
    pub cpu_quota_micros: u64,
    pub cpu_period_micros: u64,
    pub cpu_micros: u64,
    pub assigned_memory_bytes: u64,
    pub disk_write_bytes: u64,
    pub network_bytes: u64,
    pub cas_bytes: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct KernelCountersV1 {
    cpu_micros: u64,
    assigned_memory_bytes: u64,
    disk_write_bytes: u64,
    network_bytes: u64,
}

/// One-shot meter whose baseline and finish observation use the same exact
/// cgroup and network-counter directories.
#[derive(Clone, Debug)]
pub struct OcompCapacityResourceMeterV1 {
    cgroup_root: PathBuf,
    network_root: PathBuf,
    memory_limit_bytes: u64,
    cpu_quota_micros: u64,
    cpu_period_micros: u64,
    resource_cgroup_writable: bool,
    baseline: KernelCountersV1,
}

impl OcompCapacityResourceMeterV1 {
    /// Starts a meter for the caller's dedicated cgroup-v2 scope.
    ///
    /// `run_id` must occur as one complete cgroup path component. This rejects
    /// accidentally measuring the host/root cgroup or an unrelated shared CI
    /// scope.
    pub fn start_current_process(run_id: &str) -> Result<Self> {
        validate_run_id(run_id)?;
        let membership =
            fs::read_to_string("/proc/self/cgroup").wrap_err("read current cgroup membership")?;
        let relative = unified_cgroup_path(&membership)?;
        ensure!(
            relative.components().any(|component| {
                let component = component.as_os_str().to_string_lossy();
                component == format!("{run_id}.service") || component == format!("{run_id}.scope")
            }),
            "current cgroup is not the dedicated capacity run {run_id}: {}",
            relative.display()
        );
        let cgroup_root = Path::new("/sys/fs/cgroup")
            .join(relative.strip_prefix("/").unwrap_or(relative.as_path()));
        Self::start(&cgroup_root, Path::new("/sys/class/net/lo/statistics"))
    }

    pub fn start(cgroup_root: &Path, network_root: &Path) -> Result<Self> {
        validate_real_directory(cgroup_root, "capacity cgroup")?;
        validate_real_directory(network_root, "capacity network counters")?;
        let memory_limit_bytes = read_bounded_limit(&cgroup_root.join("memory.max"))?;
        let (cpu_quota_micros, cpu_period_micros) = read_cpu_limit(&cgroup_root.join("cpu.max"))?;
        let resource_cgroup_writable = probe_writable_cgroup(cgroup_root)?;
        let baseline = read_kernel_counters(cgroup_root, network_root)?;
        Ok(Self {
            cgroup_root: cgroup_root.to_path_buf(),
            network_root: network_root.to_path_buf(),
            memory_limit_bytes,
            cpu_quota_micros,
            cpu_period_micros,
            resource_cgroup_writable,
            baseline,
        })
    }

    pub fn finish(
        self,
        cas_roots: &[impl AsRef<Path>],
    ) -> Result<OcompCapacityResourceObservationV1> {
        ensure!(!cas_roots.is_empty(), "capacity run has no CAS roots");
        let final_counters = read_kernel_counters(&self.cgroup_root, &self.network_root)?;
        let cpu_micros = checked_delta(
            final_counters.cpu_micros,
            self.baseline.cpu_micros,
            "CPU usage",
        )?;
        let disk_write_bytes = checked_delta(
            final_counters.disk_write_bytes,
            self.baseline.disk_write_bytes,
            "disk writes",
        )?;
        let network_bytes = checked_delta(
            final_counters.network_bytes,
            self.baseline.network_bytes,
            "network bytes",
        )?;
        let mut cas_bytes = 0_u64;
        for root in cas_roots {
            cas_bytes = cas_bytes
                .checked_add(directory_bytes(root.as_ref())?)
                .ok_or_else(|| eyre::eyre!("capacity CAS byte count overflow"))?;
        }
        Ok(OcompCapacityResourceObservationV1 {
            cgroup_path: self.cgroup_root.to_string_lossy().into_owned(),
            resource_cgroup_writable: self.resource_cgroup_writable,
            memory_limit_bytes: self.memory_limit_bytes,
            cpu_quota_micros: self.cpu_quota_micros,
            cpu_period_micros: self.cpu_period_micros,
            cpu_micros,
            assigned_memory_bytes: final_counters.assigned_memory_bytes,
            disk_write_bytes,
            network_bytes,
            cas_bytes,
        })
    }
}

/// Measures the exact host facts used by OCM-26. Disk rates are produced by
/// Rust-owned durable writes in the target workspace, not inferred from a
/// provider label.
#[cfg(feature = "ocomp-integration")]
pub fn observe_capacity_host(workspace: &Path) -> Result<OcompCapacityHostObservationV1> {
    validate_real_directory(workspace, "capacity workspace")?;
    let operating_system = normalized_operating_system(Path::new("/etc/os-release"))?;
    let logical_cpu_count = u64::try_from(
        std::thread::available_parallelism()
            .wrap_err("read available logical CPU count")?
            .get(),
    )
    .wrap_err("logical CPU count exceeds u64")?;
    let physical_memory_bytes = physical_memory_bytes(Path::new("/proc/meminfo"))?;
    let (root_disk_bytes, _) = filesystem_capacity(Path::new("/"))?;
    let (_, free_workspace_bytes) = filesystem_capacity(workspace)?;
    let (block_iops, block_throughput_bytes_per_second) = durable_disk_probe(workspace)?;
    let pid1_is_systemd =
        fs::read_to_string("/proc/1/comm").is_ok_and(|value| value.trim() == "systemd");
    let unified_cgroup_v2 = Path::new("/sys/fs/cgroup/cgroup.controllers").is_file()
        && fs::read_to_string("/proc/self/cgroup")
            .is_ok_and(|value| unified_cgroup_path(&value).is_ok());
    Ok(OcompCapacityHostObservationV1 {
        architecture: std::env::consts::ARCH.to_owned(),
        operating_system,
        logical_cpu_count,
        physical_memory_bytes,
        root_disk_bytes,
        free_workspace_bytes,
        block_iops,
        block_throughput_bytes_per_second,
        pid1_is_systemd,
        unified_cgroup_v2,
    })
}

fn validate_run_id(run_id: &str) -> Result<()> {
    ensure!(
        (8..=96).contains(&run_id.len())
            && run_id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-'),
        "capacity run id must be 8..=96 ASCII alphanumeric/hyphen bytes"
    );
    Ok(())
}

fn unified_cgroup_path(contents: &str) -> Result<PathBuf> {
    let mut matches = contents.lines().filter_map(|line| {
        let mut fields = line.splitn(3, ':');
        (fields.next()? == "0" && fields.next()?.is_empty()).then(|| fields.next())?
    });
    let value = matches
        .next()
        .ok_or_else(|| eyre::eyre!("current process has no unified cgroup-v2 membership"))?;
    ensure!(
        matches.next().is_none(),
        "current process repeats unified cgroup-v2 membership"
    );
    let path = PathBuf::from(value);
    ensure!(
        path.is_absolute() && path != Path::new("/"),
        "capacity run cannot use the root cgroup"
    );
    Ok(path)
}

fn read_kernel_counters(cgroup_root: &Path, network_root: &Path) -> Result<KernelCountersV1> {
    Ok(KernelCountersV1 {
        cpu_micros: read_named_counter(&cgroup_root.join("cpu.stat"), "usage_usec")?,
        assigned_memory_bytes: read_u64(&cgroup_root.join("memory.peak"))?,
        disk_write_bytes: read_io_write_bytes(&cgroup_root.join("io.stat"))?,
        network_bytes: read_u64(&network_root.join("rx_bytes"))?
            .checked_add(read_u64(&network_root.join("tx_bytes"))?)
            .ok_or_else(|| eyre::eyre!("capacity network byte counter overflow"))?,
    })
}

fn read_named_counter(path: &Path, name: &str) -> Result<u64> {
    let contents = fs::read_to_string(path).wrap_err_with(|| format!("read {}", path.display()))?;
    let mut matches = contents.lines().filter_map(|line| {
        let mut fields = line.split_ascii_whitespace();
        (fields.next()? == name).then(|| fields.next())?
    });
    let value = matches
        .next()
        .ok_or_else(|| eyre::eyre!("{} has no {name} counter", path.display()))?;
    ensure!(
        matches.next().is_none(),
        "{} repeats {name}",
        path.display()
    );
    value
        .parse()
        .wrap_err_with(|| format!("parse {name} in {}", path.display()))
}

fn read_io_write_bytes(path: &Path) -> Result<u64> {
    let contents = fs::read_to_string(path).wrap_err_with(|| format!("read {}", path.display()))?;
    let mut total = 0_u64;
    for line in contents.lines().filter(|line| !line.trim().is_empty()) {
        let mut found = None;
        for field in line.split_ascii_whitespace().skip(1) {
            if let Some(value) = field.strip_prefix("wbytes=") {
                ensure!(found.is_none(), "{} repeats wbytes", path.display());
                found = Some(
                    value
                        .parse::<u64>()
                        .wrap_err_with(|| format!("parse wbytes in {}", path.display()))?,
                );
            }
        }
        let value = found.ok_or_else(|| eyre::eyre!("{} line has no wbytes", path.display()))?;
        total = total
            .checked_add(value)
            .ok_or_else(|| eyre::eyre!("capacity disk-write counter overflow"))?;
    }
    Ok(total)
}

fn read_u64(path: &Path) -> Result<u64> {
    fs::read_to_string(path)
        .wrap_err_with(|| format!("read {}", path.display()))?
        .trim()
        .parse()
        .wrap_err_with(|| format!("parse integer in {}", path.display()))
}

#[cfg(feature = "ocomp-integration")]
fn normalized_operating_system(path: &Path) -> Result<String> {
    let contents = fs::read_to_string(path).wrap_err_with(|| format!("read {}", path.display()))?;
    let mut id = None;
    let mut version = None;
    for line in contents.lines() {
        if let Some(value) = line.strip_prefix("ID=") {
            id = Some(value.trim_matches('"').to_owned());
        } else if let Some(value) = line.strip_prefix("VERSION_ID=") {
            version = Some(value.trim_matches('"').to_owned());
        }
    }
    ensure!(
        id.as_deref() == Some("ubuntu"),
        "capacity host is not Ubuntu"
    );
    let version = version.ok_or_else(|| eyre::eyre!("os-release has no VERSION_ID"))?;
    Ok(format!("Ubuntu {version}"))
}

#[cfg(feature = "ocomp-integration")]
fn physical_memory_bytes(path: &Path) -> Result<u64> {
    let contents = fs::read_to_string(path).wrap_err_with(|| format!("read {}", path.display()))?;
    let line = contents
        .lines()
        .find(|line| line.starts_with("MemTotal:"))
        .ok_or_else(|| eyre::eyre!("{} has no MemTotal", path.display()))?;
    let fields = line.split_ascii_whitespace().collect::<Vec<_>>();
    ensure!(
        fields.len() == 3 && fields[2] == "kB",
        "{} has malformed MemTotal",
        path.display()
    );
    fields[1]
        .parse::<u64>()
        .wrap_err("parse MemTotal")
        .and_then(|kilobytes| {
            kilobytes
                .checked_mul(1024)
                .ok_or_else(|| eyre::eyre!("MemTotal byte count overflow"))
        })
}

#[cfg(feature = "ocomp-integration")]
fn filesystem_capacity(path: &Path) -> Result<(u64, u64)> {
    let canonical = path
        .canonicalize()
        .wrap_err_with(|| format!("canonicalize filesystem path {}", path.display()))?;
    let stat = rustix::fs::statvfs(&canonical)
        .wrap_err_with(|| format!("statvfs {}", canonical.display()))?;
    let block_size = u128::from(stat.f_frsize);
    let total = block_size
        .checked_mul(u128::from(stat.f_blocks))
        .ok_or_else(|| eyre::eyre!("filesystem total byte count overflow"))?;
    let free = block_size
        .checked_mul(u128::from(stat.f_bavail))
        .ok_or_else(|| eyre::eyre!("filesystem free byte count overflow"))?;
    Ok((
        u64::try_from(total).wrap_err("filesystem total exceeds u64")?,
        u64::try_from(free).wrap_err("filesystem free space exceeds u64")?,
    ))
}

#[cfg(feature = "ocomp-integration")]
fn durable_disk_probe(workspace: &Path) -> Result<(u64, u64)> {
    const SEQUENTIAL_BYTES: u64 = 256 * 1024 * 1024;
    const BUFFER_BYTES: usize = 1024 * 1024;
    const SEQUENTIAL_CHUNKS: u64 = 256;
    const IOPS_THREADS: usize = 8;
    const IOPS_PER_THREAD: u64 = 1_024;
    const IOPS_BLOCK_BYTES: usize = 4 * 1024;

    let probe_root = workspace.join(format!(
        ".outbe-ocomp-capacity-disk-probe-{}",
        std::process::id()
    ));
    ensure!(
        !probe_root.exists(),
        "capacity disk probe already exists: {}",
        probe_root.display()
    );
    fs::create_dir(&probe_root)
        .wrap_err_with(|| format!("create capacity disk probe {}", probe_root.display()))?;
    let guard = ProbeDirectory(probe_root.clone());

    let sequential = probe_root.join("sequential.bin");
    let mut file = create_probe_file(&sequential)?;
    let buffer = vec![0xa5_u8; BUFFER_BYTES];
    let started = Instant::now();
    for _ in 0..SEQUENTIAL_CHUNKS {
        file.write_all(&buffer)
            .wrap_err("write sequential capacity probe")?;
    }
    file.sync_all().wrap_err("sync sequential capacity probe")?;
    let throughput = rate(SEQUENTIAL_BYTES, started.elapsed().as_nanos(), "throughput")?;
    drop(file);

    let started = Instant::now();
    std::thread::scope(|scope| -> Result<()> {
        let handles = (0..IOPS_THREADS)
            .map(|index| {
                let path = probe_root.join(format!("iops-{index}.bin"));
                scope.spawn(move || -> Result<()> {
                    let mut file = create_probe_file(&path)?;
                    let buffer = [0x5a_u8; IOPS_BLOCK_BYTES];
                    for _ in 0..IOPS_PER_THREAD {
                        file.write_all(&buffer).wrap_err("write IOPS probe")?;
                        file.sync_data().wrap_err("sync IOPS probe")?;
                    }
                    Ok(())
                })
            })
            .collect::<Vec<_>>();
        for handle in handles {
            handle
                .join()
                .map_err(|_| eyre::eyre!("capacity IOPS probe thread panicked"))??;
        }
        Ok(())
    })?;
    let operations = u64::try_from(IOPS_THREADS)
        .wrap_err("IOPS thread count exceeds u64")?
        .checked_mul(IOPS_PER_THREAD)
        .ok_or_else(|| eyre::eyre!("IOPS operation count overflow"))?;
    let iops = rate(operations, started.elapsed().as_nanos(), "IOPS")?;
    drop(guard);
    Ok((iops, throughput))
}

#[cfg(feature = "ocomp-integration")]
fn create_probe_file(path: &Path) -> Result<File> {
    OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(path)
        .wrap_err_with(|| format!("create capacity disk probe file {}", path.display()))
}

#[cfg(feature = "ocomp-integration")]
fn rate(units: u64, elapsed_nanos: u128, name: &str) -> Result<u64> {
    ensure!(elapsed_nanos > 0, "capacity {name} probe took zero time");
    let per_second = u128::from(units)
        .checked_mul(1_000_000_000)
        .ok_or_else(|| eyre::eyre!("capacity {name} rate overflow"))?
        / elapsed_nanos;
    u64::try_from(per_second).wrap_err_with(|| format!("capacity {name} rate exceeds u64"))
}

#[cfg(feature = "ocomp-integration")]
struct ProbeDirectory(PathBuf);

#[cfg(feature = "ocomp-integration")]
impl Drop for ProbeDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn read_bounded_limit(path: &Path) -> Result<u64> {
    let value = fs::read_to_string(path).wrap_err_with(|| format!("read {}", path.display()))?;
    let value = value.trim();
    ensure!(
        value != "max",
        "{} must contain a finite capacity limit",
        path.display()
    );
    value
        .parse()
        .wrap_err_with(|| format!("parse finite limit in {}", path.display()))
}

fn read_cpu_limit(path: &Path) -> Result<(u64, u64)> {
    let contents = fs::read_to_string(path).wrap_err_with(|| format!("read {}", path.display()))?;
    let fields = contents.split_ascii_whitespace().collect::<Vec<_>>();
    ensure!(
        fields.len() == 2 && fields[0] != "max",
        "{} must contain finite quota and period",
        path.display()
    );
    let quota = fields[0]
        .parse()
        .wrap_err_with(|| format!("parse CPU quota in {}", path.display()))?;
    let period = fields[1]
        .parse()
        .wrap_err_with(|| format!("parse CPU period in {}", path.display()))?;
    ensure!(
        quota > 0 && period > 0,
        "{} contains a zero CPU limit",
        path.display()
    );
    Ok((quota, period))
}

fn checked_delta(after: u64, before: u64, name: &str) -> Result<u64> {
    after
        .checked_sub(before)
        .ok_or_else(|| eyre::eyre!("capacity {name} counter moved backwards"))
}

fn probe_writable_cgroup(root: &Path) -> Result<bool> {
    let probe = root.join(format!("outbe-capacity-write-probe-{}", std::process::id()));
    ensure!(
        !probe.exists(),
        "capacity cgroup write probe already exists: {}",
        probe.display()
    );
    fs::create_dir(&probe)
        .wrap_err_with(|| format!("create delegated cgroup probe {}", probe.display()))?;
    fs::remove_dir(&probe)
        .wrap_err_with(|| format!("remove delegated cgroup probe {}", probe.display()))?;
    Ok(true)
}

fn directory_bytes(root: &Path) -> Result<u64> {
    validate_real_directory(root, "capacity CAS root")?;
    let mut total = 0_u64;
    for entry in WalkDir::new(root).follow_links(false) {
        let entry = entry.wrap_err_with(|| format!("walk capacity CAS root {}", root.display()))?;
        let metadata = fs::symlink_metadata(entry.path())
            .wrap_err_with(|| format!("inspect capacity CAS member {}", entry.path().display()))?;
        if metadata.file_type().is_symlink() {
            bail!(
                "capacity CAS root contains a symlink: {}",
                entry.path().display()
            );
        }
        if metadata.is_file() {
            total = total
                .checked_add(metadata.len())
                .ok_or_else(|| eyre::eyre!("capacity CAS byte count overflow"))?;
        }
    }
    Ok(total)
}

fn validate_real_directory(path: &Path, name: &str) -> Result<()> {
    let metadata = fs::symlink_metadata(path)
        .wrap_err_with(|| format!("inspect {name} {}", path.display()))?;
    ensure!(
        metadata.file_type().is_dir() && !metadata.file_type().is_symlink(),
        "{name} is not a real directory: {}",
        path.display()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unified_cgroup_membership_is_exact_and_non_root() {
        assert_eq!(
            unified_cgroup_path("0::/system.slice/outbe-ocomp-capacity-01.scope\n").unwrap(),
            PathBuf::from("/system.slice/outbe-ocomp-capacity-01.scope")
        );
        assert!(unified_cgroup_path("0::/\n").is_err());
        assert!(unified_cgroup_path("1:name:/legacy\n").is_err());
    }
}
