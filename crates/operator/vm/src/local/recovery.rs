//! VMM discovery by exact name and recovery of interrupted `Creating` intents through
//! `CREATE_TOKEN_ANNOTATION`.

use std::{
    collections::BTreeSet,
    sync::atomic::AtomicBool,
    time::{Duration, Instant},
};

use eyre::{bail, ensure, Result};

use super::{
    check_canceled, state::Mismatches, vmm_ops::Poll, LocalVmState, CREATE_TOKEN_ANNOTATION,
    LOCK_POLL_INTERVAL, OWNER_TAG_ANNOTATION,
};
use crate::vmm::{StatusRequest, VmInfo, VmmClient, VmmTransport};

pub(super) const STATUS_DISCOVERY_PAGE_SIZE: u32 = 100;
const MAX_STATUS_DISCOVERY_PAGES: u32 = 100;

/// Result of polling the VMM for the VM of a journaled `Creating` intent.
pub(super) struct CreatingIntentProbe {
    /// Create token journaled with the intent.
    pub(super) token: String,
    /// Token-matched VM whose configuration matches the journal, if one appeared in time.
    pub(super) adopted: Option<VmInfo>,
}

/// Grace deadline for `Creating` recovery polling, bounded by the operation deadline.
pub(super) fn compute_grace_deadline(grace: Duration, op_deadline: Option<Instant>) -> Instant {
    let by_grace = Instant::now() + grace;
    match op_deadline {
        Some(op) => by_grace.min(op),
        None => by_grace,
    }
}

/// Poll the VMM during `poll` for the VM of the `Creating` intent in `state` and verify any match
/// against the journal before the caller adopts it.
pub(super) fn probe_creating_intent<T: VmmTransport>(
    vmm: &VmmClient<T>,
    state: &LocalVmState,
    poll: Poll,
    cancel: Option<&AtomicBool>,
) -> Result<CreatingIntentProbe> {
    let token = state
        .create_token
        .clone()
        .ok_or_else(|| eyre::eyre!("creating state missing create_token"))?;
    let adopted = poll_for_owned_creating_vm(vmm, state, &token, poll, cancel)?;
    if let Some(recovered) = &adopted {
        verify_adopted_vm_matches_state(recovered, state)?;
    }
    Ok(CreatingIntentProbe { token, adopted })
}

/// Paginate brief `Status` filtered by `keyword = name` and return deduplicated candidate VM IDs
/// whose exact name equals `name`.
///
/// Uses `brief = true` and `page_size = 100` so unrelated VMs and large compose/env blobs on the
/// host never bloat a single `Status` response past the 32 MiB HTTP cap.
fn list_exact_name_candidate_ids<T: VmmTransport>(
    vmm: &VmmClient<T>,
    name: &str,
    deadline: Option<Instant>,
) -> Result<Vec<String>> {
    let mut candidate_ids = Vec::new();
    let mut seen_ids = BTreeSet::new();

    for page in 1..=MAX_STATUS_DISCOVERY_PAGES {
        let resp = vmm.status_with_deadline(
            &StatusRequest {
                brief: true,
                keyword: name.to_owned(),
                page,
                page_size: STATUS_DISCOVERY_PAGE_SIZE,
                ..StatusRequest::default()
            },
            deadline,
        )?;
        let count = resp.vms.len() as u32;
        let exact = resp
            .vms
            .into_iter()
            .filter(|vm| vm.name == name && !vm.id.is_empty())
            .map(|vm| vm.id);
        candidate_ids.extend(exact.filter(|id| seen_ids.insert(id.clone())));
        if is_last_status_page(count, page, resp.total) {
            return Ok(candidate_ids);
        }
    }

    bail!(
        "VMM Status pagination exceeded hard cap ({} pages / {} entries) while querying keyword `{name}`",
        MAX_STATUS_DISCOVERY_PAGES,
        MAX_STATUS_DISCOVERY_PAGES * STATUS_DISCOVERY_PAGE_SIZE
    );
}

/// True when a `Status` page is short, or when the pages so far cover the reported total.
fn is_last_status_page(count: u32, page: u32, total: u32) -> bool {
    let covers_total = total > 0 && page.saturating_mul(STATUS_DISCOVERY_PAGE_SIZE) >= total;
    count < STATUS_DISCOVERY_PAGE_SIZE || covers_total
}

/// Refuse to create `name` when any VM with the same exact name is registered on the VMM.
pub(super) fn ensure_no_foreign_name_collision<T: VmmTransport>(
    vmm: &VmmClient<T>,
    name: &str,
    deadline: Option<Instant>,
) -> Result<()> {
    let candidates = list_exact_name_candidate_ids(vmm, name, deadline)?;
    if let Some(collision_id) = candidates.first() {
        bail!(
            "refusing to create VM `{name}`: a VM with the same name ({collision_id}) already exists on the local VMM"
        );
    }
    Ok(())
}

/// Search filtered brief `Status` pages for exact-name candidates, then fetch full `GetInfo` ONLY
/// for those candidates and match `(name, CREATE_TOKEN_ANNOTATION)`.
///
/// Security note: `CREATE_TOKEN_ANNOTATION` is a bearer recovery marker readable by any client
/// with access to the VMM Unix socket; callers MUST also pass any returned [`VmInfo`] through
/// [`verify_adopted_vm_matches_state`] before adopting it.
fn find_owned_creating_vm<T: VmmTransport>(
    vmm: &VmmClient<T>,
    name: &str,
    expected_token: &str,
    deadline: Option<Instant>,
) -> Result<Option<VmInfo>> {
    let candidate_ids = list_exact_name_candidate_ids(vmm, name, deadline)?;
    let mut matches = Vec::new();
    for id in candidate_ids {
        let info = vmm.get_info_with_deadline(&id, deadline)?.info;
        matches.extend(info.filter(|info| {
            info.name == name && info.annotation(CREATE_TOKEN_ANNOTATION) == Some(expected_token)
        }));
    }
    ensure!(
        matches.len() <= 1,
        "multiple VMM entries matched create token for VM `{name}`; resolve manually"
    );
    Ok(matches.into_iter().next())
}

/// Poll `Status`/`GetInfo` until `poll.deadline` for a token-matched VM created by an interrupted
/// `CreateVm` request, returning `Ok(None)` if still absent when the grace window expires.
///
/// With a cancel flag, each sleep is at most `LOCK_POLL_INTERVAL` so a signal stops polling fast.
fn poll_for_owned_creating_vm<T: VmmTransport>(
    vmm: &VmmClient<T>,
    state: &LocalVmState,
    expected_token: &str,
    poll: Poll,
    cancel: Option<&AtomicBool>,
) -> Result<Option<VmInfo>> {
    let poll = match cancel {
        Some(_) => Poll {
            interval: poll.interval.min(LOCK_POLL_INTERVAL),
            ..poll
        },
        None => poll,
    };
    loop {
        check_canceled(cancel, "polling VMM for in-flight Creating VM")?;
        let found = find_owned_creating_vm(vmm, &state.name, expected_token, Some(poll.deadline))?;
        if found.is_some() || poll.expired() {
            return Ok(found);
        }
        poll.sleep();
    }
}

/// Verify that a candidate VM recovered via `CREATE_TOKEN_ANNOTATION` matches our journaled
/// `name`, `image`, `vcpu`, `memory`, and `app_id` before adopting it.
fn verify_adopted_vm_matches_state(info: &VmInfo, state: &LocalVmState) -> Result<()> {
    let mut mismatches = Mismatches::default();
    mismatches.check(info.name != state.name, || {
        format!("name (`{}` vs `{}`)", info.name, state.name)
    });
    let Some(cfg) = &info.configuration else {
        bail!(
            "refusing to adopt VM {} for `{}`: VMM GetInfo omitted configuration block",
            info.id,
            state.name
        );
    };
    mismatches.check(cfg.image != state.image_name, || {
        format!("image (`{}` vs `{}`)", cfg.image, state.image_name)
    });
    mismatches.check(cfg.vcpu != state.vcpu, || {
        format!("vcpu ({} vs {})", cfg.vcpu, state.vcpu)
    });
    let expected_memory_mb = state.memory_gib * 1024;
    mismatches.check(cfg.memory != expected_memory_mb, || {
        format!("memory ({} MiB vs {} MiB)", cfg.memory, expected_memory_mb)
    });
    let actual_app_id = info.effective_app_id();
    mismatches.check(actual_app_id != Some(state.app_id.as_str()), || {
        let actual = actual_app_id.map_or_else(|| "missing".to_owned(), |id| format!("`{id}`"));
        format!("app_id ({actual} vs `{}`)", state.app_id)
    });
    let actual_owner_tag = info.annotation(OWNER_TAG_ANNOTATION);
    mismatches.check(actual_owner_tag != state.owner_tag.as_deref(), || {
        format!(
            "owner_tag ({:?} vs {:?})",
            actual_owner_tag,
            state.owner_tag.as_deref()
        )
    });
    ensure!(
        mismatches.is_empty(),
        "refusing to adopt token-matched VM {} for `{}`: VMM configuration does not match journal ({})",
        info.id,
        state.name,
        mismatches.join()
    );
    Ok(())
}
