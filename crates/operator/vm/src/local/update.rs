//! `vm update`: switch a stopped VM to a new guest image with graceful shutdown only.

use std::{
    path::PathBuf,
    sync::atomic::AtomicBool,
    time::{Duration, Instant},
};

use eyre::{ensure, Context, Result};
use serde::{Deserialize, Serialize};

use super::{
    check_canceled,
    state::{validate_expected_vm_id, verify_expected_vm_id},
    store::VmJournal,
    vmm_ops::{require_owned_vmm_info, wait_until_running, wait_until_stopped, Poll},
    LocalVmPhase, LocalVmState, LocalVmStore,
};
use crate::{
    image::{publish_local_image_locked_cancellable, LocalImageSource},
    vmm::{UpdateVmRequest, VmInfo, VmmClient, VmmTransport},
};

/// Parameters for `outbe-up vm update` (graceful stop, switch guest image via `UpdateVm`, start
/// with the same `hda.img` data disk; never uses forced `StopVm`).
#[derive(Clone, Debug)]
pub struct UpdateLocalVmOptions {
    pub name: String,
    pub expect_vm_id: Option<String>,
    /// Local guest image directory OR name of an already-published image under `<vmm_home>/image/`.
    pub image: PathBuf,
    pub image_name: Option<String>,
    pub flavor: String,
    pub vmm_home: PathBuf,
    pub stop_timeout: Duration,
    pub poll_interval: Duration,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct UpdateLocalVmOutput {
    pub name: String,
    pub vm_id: String,
    pub previous_image_name: String,
    pub previous_os_image_hash: String,
    pub image_name: String,
    pub os_image_hash: String,
    pub image_reused: bool,
    pub already_updated: bool,
    pub vmm_status: String,
}

/// Target guest image of one update.
struct TargetImage {
    name: String,
    os_image_hash: String,
}

impl TargetImage {
    /// True when both the VMM and the journal already run this VM on the target image.
    fn is_running_on(&self, info: &VmInfo, state: &LocalVmState) -> bool {
        let vmm_on_target = info.running && info.image_name() == Some(self.name.as_str());
        let journal_on_target =
            state.image_name == self.name && state.os_image_hash == self.os_image_hash;
        vmm_on_target && journal_on_target
    }
}

/// One update run: the VMM client, the per-VM journal, the options, and the cancel flag.
struct UpdateRun<'a, T: VmmTransport> {
    vmm: &'a VmmClient<T>,
    journal: VmJournal<'a>,
    options: &'a UpdateLocalVmOptions,
    cancel: Option<&'a AtomicBool>,
}

/// Update one locally recorded VM to a new guest OS image while preserving its VM UUID, MAC,
/// networking, annotations, and `hda.img` data disk.
///
/// Strictly uses graceful guest shutdown (`ShutdownVm`) and NEVER calls forced `StopVm` (see
/// `/home/ubuntu/tdx/12-defect-pending-dkg-snapshot.md`). Idempotent: returns
/// `already_updated = true` without restarting when the VM is already running on the target image.
pub fn update_local_vm<T: VmmTransport>(
    store: &LocalVmStore,
    vmm: &VmmClient<T>,
    options: &UpdateLocalVmOptions,
) -> Result<UpdateLocalVmOutput> {
    update_local_vm_cancellable(store, vmm, options, None)
}

/// Same as [`update_local_vm`], honoring an optional cooperative cancellation flag.
pub fn update_local_vm_cancellable<T: VmmTransport>(
    store: &LocalVmStore,
    vmm: &VmmClient<T>,
    options: &UpdateLocalVmOptions,
    cancel: Option<&AtomicBool>,
) -> Result<UpdateLocalVmOutput> {
    validate_expected_vm_id(options.expect_vm_id.as_deref())?;
    // F5: Use separate deadlines for image publishing/locking, graceful shutdown, UpdateVm, and
    // post-start running confirmation so a slow image publish never eats into `stop_timeout`.
    let prep_budget = options.stop_timeout.max(Duration::from_secs(60));
    let publish_deadline = Instant::now() + prep_budget;

    // Resolve and lock target image BEFORE stopping the VM so missing/corrupt images fail closed
    // without causing any downtime.
    let (image_dir, effective_image_name) = resolve_update_image_source(options)?;
    let published = publish_local_image_locked_cancellable(
        &options.vmm_home,
        LocalImageSource {
            image_dir: &image_dir,
            image_name: effective_image_name.as_deref(),
            flavor_label: &options.flavor,
        },
        Some(publish_deadline),
        cancel,
    )?;
    let _image_lock = published.lock;
    let target = TargetImage {
        name: published.published.name,
        os_image_hash: published.published.digest,
    };
    let image_reused = published.published.reused;

    let lock_deadline = Instant::now() + prep_budget;
    let guard = store.lock_vm_with_options(&options.name, Some(lock_deadline), cancel)?;
    let mut state = store.load_locked(&guard, &options.name)?;
    verify_expected_vm_id(&state, options.expect_vm_id.as_deref())?;
    ensure!(
        state.phase == LocalVmPhase::Ready,
        "VM `{}` ({}) is in phase `{:?}`, not `Ready`; refusing to update image",
        options.name,
        state.vm_id,
        state.phase
    );

    let info = require_owned_vmm_info(vmm, &state, Some(lock_deadline))?;
    let previous_image_name = info
        .image_name()
        .map(str::to_owned)
        .unwrap_or_else(|| state.image_name.clone());
    let previous_os_image_hash = state.os_image_hash.clone();

    // Idempotent fast path: already running on target image.
    let already_updated = target.is_running_on(&info, &state);
    let vmm_status = if already_updated {
        info.status
    } else {
        let run = UpdateRun {
            vmm,
            journal: VmJournal {
                store,
                guard: &guard,
            },
            options,
            cancel,
        };
        run.apply(&mut state, &info, &target, &previous_image_name)?
    };

    Ok(UpdateLocalVmOutput {
        name: state.name,
        vm_id: state.vm_id,
        previous_image_name,
        previous_os_image_hash,
        image_name: target.name,
        os_image_hash: target.os_image_hash,
        image_reused,
        already_updated,
        vmm_status,
    })
}

impl<T: VmmTransport> UpdateRun<'_, T> {
    /// Stop, switch the image, persist the new identity, and start; return the final VMM status.
    fn apply(
        &self,
        state: &mut LocalVmState,
        info: &VmInfo,
        target: &TargetImage,
        previous_image_name: &str,
    ) -> Result<String> {
        // 1. Strictly graceful shutdown if currently running (never `StopVm`), with its own full
        //    `stop_timeout` budget starting now.
        if !info.is_stopped() {
            self.shut_down(state, &target.name)?;
        }

        // 2. Switch image via VMM `UpdateVm` if VMM manifest does not already name the target
        //    image.
        self.switch_image(state, &target.name, previous_image_name)?;

        // Persist new image identity BEFORE starting so local state matches VMM even if start
        // fails.
        state.image_name = target.name.clone();
        state.os_image_hash = target.os_image_hash.clone();
        self.journal.record(
            state,
            "updated-image",
            Some(format!(
                "image={} hash={}",
                target.name, target.os_image_hash
            )),
        )?;

        // 3. Start VM with the exact same `hda.img` data disk and wait until VMM confirms running
        //    (F6).
        self.start(state, &target.name)
    }

    fn shut_down(&self, state: &mut LocalVmState, target_image_name: &str) -> Result<()> {
        let options = self.options;
        let poll = Poll {
            deadline: Instant::now() + options.stop_timeout,
            interval: options.poll_interval,
        };
        check_canceled(self.cancel, "requesting graceful shutdown for vm update")?;
        self.journal.record(
            state,
            "updating-shutting-down",
            Some(format!("target_image={target_image_name}")),
        )?;
        self.vmm
            .shutdown_vm_with_deadline(&state.vm_id, Some(poll.deadline))
            .wrap_err_with(|| {
                format!(
                    "gracefully shut down VM `{}` ({}) for image update (forced StopVm is never used)",
                    state.name, state.vm_id
                )
            })?;
        wait_until_stopped(self.vmm, state, poll, false).wrap_err_with(|| {
            format!(
                "VM `{}` ({}) did not stop gracefully within {}s; aborting image update without force-stop (pending-DKG safety)",
                state.name,
                state.vm_id,
                options.stop_timeout.as_secs()
            )
        })?;
        Ok(())
    }

    fn switch_image(
        &self,
        state: &mut LocalVmState,
        target_image_name: &str,
        previous_image_name: &str,
    ) -> Result<()> {
        let update_deadline = Instant::now() + Duration::from_secs(60);
        let current_info = require_owned_vmm_info(self.vmm, state, Some(update_deadline))?;
        ensure!(
            current_info.is_stopped(),
            "VM `{}` ({}) is not stopped before UpdateVm (status={}, running={})",
            state.name,
            state.vm_id,
            current_info.status,
            current_info.running
        );
        if current_info.image_name() == Some(target_image_name) {
            return Ok(());
        }
        check_canceled(self.cancel, "calling VMM UpdateVm")?;
        self.journal.record(
            state,
            "updating-image",
            Some(format!("from={previous_image_name} to={target_image_name}")),
        )?;
        self.vmm
            .update_vm_with_deadline(
                &UpdateVmRequest {
                    id: state.vm_id.clone(),
                    image: Some(target_image_name.to_owned()),
                },
                Some(update_deadline),
            )
            .wrap_err_with(|| {
                format!(
                    "update VM `{}` ({}) image to `{target_image_name}`",
                    state.name, state.vm_id
                )
            })?;
        let verified = require_owned_vmm_info(self.vmm, state, Some(update_deadline))?;
        ensure!(
            verified.image_name() == Some(target_image_name),
            "VMM UpdateVm for `{}` ({}) did not record target image `{target_image_name}` (got {:?})",
            state.name,
            state.vm_id,
            verified.image_name()
        );
        Ok(())
    }

    fn start(&self, state: &mut LocalVmState, target_image_name: &str) -> Result<String> {
        let start_budget = self
            .options
            .stop_timeout
            .clamp(Duration::from_millis(50), Duration::from_secs(60));
        let poll = Poll {
            deadline: Instant::now() + start_budget,
            interval: self.options.poll_interval,
        };
        check_canceled(self.cancel, "starting VM after image update")?;
        self.vmm
            .start_vm_with_deadline(&state.vm_id, Some(poll.deadline))
            .wrap_err_with(|| {
                format!(
                    "start VM `{}` ({}) after switching image to `{target_image_name}`",
                    state.name, state.vm_id
                )
            })?;
        let post_status = wait_until_running(self.vmm, state, poll, self.cancel)?;
        self.journal.record(
            state,
            "updated-started",
            Some(format!("status={post_status}")),
        )?;
        Ok(post_status)
    }
}

fn resolve_update_image_source(
    options: &UpdateLocalVmOptions,
) -> Result<(PathBuf, Option<String>)> {
    if options.image.is_dir() {
        return Ok((options.image.clone(), options.image_name.clone()));
    }
    let raw = options
        .image
        .to_str()
        .ok_or_else(|| eyre::eyre!("non-UTF-8 image path/name: {}", options.image.display()))?;
    ensure!(
        is_plain_image_name(raw),
        "guest image directory does not exist: {}",
        options.image.display()
    );
    let candidate = options.vmm_home.join("image").join(raw);
    ensure!(
        candidate.is_dir(),
        "guest image `{raw}` not found under {} (pass a local image directory or an already-published image name)",
        options.vmm_home.join("image").display()
    );
    let effective_name = options.image_name.clone().unwrap_or_else(|| raw.to_owned());
    Ok((candidate, Some(effective_name)))
}

/// True when `raw` is one path component that can name an image under `<vmm_home>/image/`.
fn is_plain_image_name(raw: &str) -> bool {
    let has_separator = raw.contains('/') || raw.contains('\\');
    !raw.is_empty() && !has_separator && raw != "." && raw != ".."
}
