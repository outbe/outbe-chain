//! Virtualization and TEE platform detection (KVM, vhost-vsock, AMD SEV-SNP, Intel TDX, QGS).

use std::{fs, path::Path};

use super::{probe::query_systemd_unit, HostEnv, PlatformStatus, QgsStatus};

const QGSD_SERVICE: &str = "qgsd.service";
const PCCS_SERVICE: &str = "pccs.service";

pub(super) fn detect_platform(env: &HostEnv) -> PlatformStatus {
    let kvm_path = env.path("/dev/kvm");
    let kvm_available = kvm_path.exists();
    let kvm_accessible = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&kvm_path)
        .is_ok();
    let vhost_vsock_available = env.path("/dev/vhost-vsock").exists();

    let sev_snp_enabled = read_sysfs_bool(&env.path("/sys/module/kvm_amd/parameters/sev_snp"));
    let tdx_enabled = read_sysfs_bool(&env.path("/sys/module/kvm_intel/parameters/tdx"));

    let tee_kind = if sev_snp_enabled {
        "amd-sev-snp".to_owned()
    } else if tdx_enabled {
        "intel-tdx".to_owned()
    } else {
        "none".to_owned()
    };

    let qgs_unit = query_systemd_unit(QGSD_SERVICE);
    let pccs_unit = query_systemd_unit(PCCS_SERVICE);
    let qgs = QgsStatus {
        required: tdx_enabled,
        service_name: QGSD_SERVICE.to_owned(),
        unit_enabled: qgs_unit.is_enabled(),
        unit_active: qgs_unit.is_active(),
        pccs_unit_active: pccs_unit.is_active(),
    };

    PlatformStatus {
        kvm_available,
        kvm_accessible,
        vhost_vsock_available,
        tee_kind,
        sev_snp_enabled,
        tdx_enabled,
        qgs,
    }
}

fn read_sysfs_bool(path: &Path) -> bool {
    fs::read_to_string(path)
        .map(|raw| matches!(raw.trim(), "Y" | "y" | "1" | "true"))
        .unwrap_or(false)
}
