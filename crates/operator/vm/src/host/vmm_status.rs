//! Local dstack VMM reachability, capability, and drift detection.

use std::path::Path;

use super::{probe::query_systemd_unit, VmmStatus};
use crate::vmm::{StatusRequest, VmInfo, VmmClient, VmmTransport};

const DSTACK_VMM_SERVICE: &str = "dstack-vmm.service";
const STATUS_PAGE_SIZE: u32 = 100;
const MAX_STATUS_PAGES: u32 = 100;

pub(super) fn detect_vmm_status<T: VmmTransport>(
    vmm: &VmmClient<T>,
    socket_path: &Path,
) -> VmmStatus {
    let unit = query_systemd_unit(DSTACK_VMM_SERVICE);
    let version_res = vmm.version().ok();
    let meta_res = vmm.get_meta_typed().ok();
    let vms_res = list_all_vms_best_effort(vmm);
    let reachable = version_res.is_some() || meta_res.is_some() || vms_res.is_some();

    let (running_vms, total_vms) = match vms_res {
        Some(vms) => {
            let running = vms.iter().filter(|v| v.running).count();
            (Some(running), Some(vms.len()))
        }
        None => (None, None),
    };

    let mut drift_notes = Vec::new();
    if reachable && unit.is_enabled() && !unit.is_active() {
        drift_notes.push(
            "dstack-vmm.service is enabled/inactive while live VMM responds on UDS; do not start/restart dstack-vmm.service"
                .to_owned(),
        );
    }
    if meta_res
        .as_ref()
        .is_some_and(|m| m.networking.default_mode == "user")
    {
        drift_notes.push(
            "live VMM reports default_mode=user; pass --bridge <name> explicitly to outbe-up vm create"
                .to_owned(),
        );
    }

    VmmStatus {
        socket_path: socket_path.to_path_buf(),
        reachable,
        version: version_res
            .as_ref()
            .map(|v| v.version.clone())
            .filter(|s| !s.is_empty()),
        rev: version_res
            .as_ref()
            .map(|v| v.rev.clone())
            .filter(|s| !s.is_empty()),
        default_mode: meta_res
            .as_ref()
            .map(|m| m.networking.default_mode.clone())
            .filter(|s| !s.is_empty()),
        supported_modes: meta_res
            .as_ref()
            .map(|m| m.networking.supported_modes.clone())
            .unwrap_or_default(),
        default_bridge: meta_res
            .as_ref()
            .map(|m| m.networking.default_bridge.clone())
            .filter(|s| !s.is_empty()),
        max_cvm_number: meta_res.as_ref().map(|m| m.resources.max_cvm_number),
        max_allocable_vcpu: meta_res.as_ref().map(|m| m.resources.max_allocable_vcpu),
        max_allocable_memory_in_mb: meta_res
            .as_ref()
            .map(|m| m.resources.max_allocable_memory_in_mb),
        running_vms,
        total_vms,
        systemd_unit_enabled: unit.is_enabled(),
        systemd_unit_active: unit.is_active(),
        drift_notes,
    }
}

fn list_all_vms_best_effort<T: VmmTransport>(vmm: &VmmClient<T>) -> Option<Vec<VmInfo>> {
    let mut all = Vec::new();
    for page in 1..=MAX_STATUS_PAGES {
        let resp = vmm
            .status(&StatusRequest {
                ids: Vec::new(),
                brief: false,
                keyword: String::new(),
                page,
                page_size: STATUS_PAGE_SIZE,
                status: String::new(),
            })
            .ok()?;
        let count = resp.vms.len();
        all.extend(resp.vms);
        if count < STATUS_PAGE_SIZE as usize || (resp.total > 0 && all.len() >= resp.total as usize)
        {
            return Some(all);
        }
    }
    Some(all)
}
