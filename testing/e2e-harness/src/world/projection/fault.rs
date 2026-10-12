//! Scenario-owned RocksDB projection write faults.

#[cfg(feature = "ocomp-integration")]
use std::fs;
#[cfg(feature = "ocomp-integration")]
use std::io::Write;
#[cfg(feature = "ocomp-integration")]
use std::os::unix::fs::MetadataExt as _;
use std::process::Command;

#[cfg(feature = "ocomp-integration")]
use eyre::eyre;
use eyre::Result;

use super::ProjectionFixture;
use crate::internal::config::Config;

impl ProjectionFixture {
    #[cfg(feature = "ocomp-integration")]
    pub(crate) fn mount_rocksdb_fault_volume(&self, validator: usize) -> Result<()> {
        eyre::ensure!(
            self.cfg.projection_backend == crate::env::ProjectionBackend::Rocksdb,
            "RocksDB write fault requires a RocksDB projection"
        );
        let mut mounted = self.fault_mount.lock().expect("fault mount mutex");
        eyre::ensure!(mounted.is_none(), "projection fault volume already mounted");
        let path = self.cfg.validator_dir(validator).join("data/offchain");
        fs::create_dir_all(&path)?;
        let owner = fs::metadata(self.cfg.validator_dir(validator))?;
        let options = format!(
            "nodev,nosuid,noexec,size=64m,uid={},gid={}",
            owner.uid(),
            owner.gid()
        );
        let mut command = privileged_command(&self.cfg, "mount");
        let output = command
            .args(["-t", "tmpfs", "-o", &options, "tmpfs"])
            .arg(&path)
            .output()?;
        eyre::ensure!(
            output.status.success(),
            "mount scenario RocksDB volume: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        *mounted = Some(path);
        Ok(())
    }

    #[cfg(feature = "ocomp-integration")]
    pub(crate) fn exhaust_rocksdb_fault_volume(&self) -> Result<()> {
        let mounted = self.fault_mount.lock().expect("fault mount mutex");
        let path = mounted
            .as_ref()
            .ok_or_else(|| eyre!("no scenario RocksDB fault volume"))?;
        let filler = path.join("e2e-enospc-filler");
        let mut file = fs::File::create(&filler)?;
        let chunk = [0_u8; 1024 * 1024];
        let mut exhausted = false;
        for size in [chunk.len(), 4096, 1] {
            let bytes = &chunk[..size];
            loop {
                match file.write_all(bytes) {
                    Ok(()) => {}
                    Err(error) if error.raw_os_error() == Some(libc::ENOSPC) => {
                        exhausted = true;
                        break;
                    }
                    Err(error) => return Err(error.into()),
                }
            }
        }
        eyre::ensure!(exhausted, "RocksDB fault volume never reached ENOSPC");
        Ok(())
    }

    #[cfg(feature = "ocomp-integration")]
    pub(crate) fn restore_rocksdb_fault_volume(&self) -> Result<()> {
        let mounted = self.fault_mount.lock().expect("fault mount mutex");
        let path = mounted
            .as_ref()
            .ok_or_else(|| eyre!("no scenario RocksDB fault volume"))?;
        fs::remove_file(path.join("e2e-enospc-filler"))?;
        Ok(())
    }

    pub(crate) fn teardown_fault_mount(&self) -> Result<()> {
        let mut mounted = self.fault_mount.lock().expect("fault mount mutex");
        let Some(path) = mounted.as_ref() else {
            return Ok(());
        };
        let output = privileged_command(&self.cfg, "umount").arg(path).output()?;
        eyre::ensure!(
            output.status.success(),
            "unmount scenario RocksDB volume: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        *mounted = None;
        Ok(())
    }
}

fn privileged_command(cfg: &Config, program: &str) -> Command {
    if cfg.sudo {
        let mut command = Command::new("sudo");
        command.arg("-n").arg(program);
        command
    } else {
        Command::new(program)
    }
}
