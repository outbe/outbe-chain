//! `vm create`: journaled, resumable creation of one stopped local VM.
//!
//! Phases advance `Creating -> Created -> MacRecorded -> DiskStaged -> Ready`, and each phase is
//! saved before the next fallible step, so a rerun with the same inputs resumes where the last
//! run stopped.

use std::{
    collections::BTreeMap,
    fs,
    net::Ipv4Addr,
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
    time::{Duration, Instant},
};

use eyre::{bail, ensure, Context, Result};
use serde::{Deserialize, Serialize};

use super::{
    check_canceled,
    lock::StagingLockGuard,
    recovery::{compute_grace_deadline, ensure_no_foreign_name_collision, probe_creating_intent},
    state::{generate_create_token, validate_flavor, validate_owner_tag, Mismatches},
    store::VmJournal,
    validate_vm_name,
    vmm_ops::Poll,
    LocalVmPhase, LocalVmState, LocalVmStore, CREATE_TOKEN_ANNOTATION, LOCAL_VM_STATE_VERSION,
    OWNER_TAG_ANNOTATION,
};
use crate::{
    disk::{sha256_sparse_file, stage_local_disk_cancellable},
    image::{publish_local_image_locked_cancellable, LocalImageSource},
    nat::validate_iface_name,
    vmm::{
        calc_app_id, extract_primary_mac, CreateVmRequest, NetworkingConfig, VmmClient,
        VmmTransport, APP_COMPOSE_JSON,
    },
};

#[derive(Clone, Debug)]
pub struct CreateLocalVmOptions {
    pub name: String,
    pub owner_tag: Option<String>,
    pub image_dir: PathBuf,
    pub image_name: Option<String>,
    pub flavor: String,
    pub disk_source: PathBuf,
    /// Optional staging name under `<state-dir>/staging/<name>` (`--disk-staging <name>`).
    /// When set, `create_local_vm*` holds `lock_staging(name)` BEFORE `lock_vm` across
    /// resolution, SHA-256 hashing, `CreateVm`, disk conversion, and optional consumption so the
    /// hashed bytes are guaranteed to match the converted bytes.
    pub disk_staging_name: Option<String>,
    /// When `true` (requires `disk_staging_name.is_some()`), deletes `<state-dir>/staging/<name>`
    /// ONLY after `LocalVmPhase::Ready` is durably saved.
    pub consume_staging: bool,
    pub vcpu: u32,
    pub memory_gib: u32,
    pub disk_gib: u32,
    pub bridge: String,
    pub app_compose_path: Option<PathBuf>,
    pub intended_ip: Option<Ipv4Addr>,
    pub no_tee: bool,
    pub vmm_socket: PathBuf,
    pub vmm_home: PathBuf,
    /// Allow issuing a new `CreateVm` when a prior `Creating` intent did not appear within the
    /// grace polling window.
    pub retry_create: bool,
    /// Optional overall timeout bounding lock acquisition, recovery grace polling, and RPCs.
    pub timeout: Option<Duration>,
    /// Grace polling window when resolving an existing `Creating` intent (default 30s, bounded by
    /// `timeout` if set).
    pub recovery_grace: Duration,
    /// Poll interval used during `Creating` grace polling.
    pub poll_interval: Duration,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CreateLocalVmOutput {
    pub name: String,
    pub vm_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_tag: Option<String>,
    pub image_name: String,
    pub os_image_hash: String,
    pub image_reused: bool,
    pub mac: String,
    pub bridge: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub intended_ip: Option<Ipv4Addr>,
    pub vcpu: u32,
    pub memory_gib: u32,
    pub disk_gib: u32,
    pub phase: LocalVmPhase,
    #[serde(default)]
    pub staging_consumed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub warning: Option<String>,
    pub state_file: PathBuf,
}

/// Staging lock held from disk resolution until the staged disk is consumed or released.
struct HeldStaging {
    name: String,
    lock: StagingLockGuard,
}

/// Inputs resolved for one create run that the journal records as immutable parameters.
struct ResolvedCreateInputs {
    disk_source: PathBuf,
    disk_source_sha256: String,
    compose_file: String,
    app_id: String,
    image_name: String,
    os_image_hash: String,
}

/// One create run: the VMM client, the per-VM journal, the options, the resolved inputs, the
/// operation deadline, and the cancel flag.
struct CreateRun<'a, T: VmmTransport> {
    vmm: &'a VmmClient<T>,
    journal: VmJournal<'a>,
    options: &'a CreateLocalVmOptions,
    inputs: &'a ResolvedCreateInputs,
    op_deadline: Option<Instant>,
    cancel: Option<&'a AtomicBool>,
}

/// Create one stopped local VM, publish its guest image under a digest-qualified name,
/// stage its qcow2 data disk locally, and journal every transition to its per-VM state file.
pub fn create_local_vm<T: VmmTransport>(
    store: &LocalVmStore,
    vmm: &VmmClient<T>,
    options: &CreateLocalVmOptions,
) -> Result<CreateLocalVmOutput> {
    let cancel = AtomicBool::new(false);
    create_local_vm_cancellable(store, vmm, options, &cancel)
}

/// Same as [`create_local_vm`], honoring a signal-driven cancellation flag across lock wait,
/// pre-RPC checkpoints, recovery grace polling, and `qemu-img` execution.
pub fn create_local_vm_cancellable<T: VmmTransport>(
    store: &LocalVmStore,
    vmm: &VmmClient<T>,
    options: &CreateLocalVmOptions,
    cancel: &AtomicBool,
) -> Result<CreateLocalVmOutput> {
    create_local_vm_with_disk_stager_cancellable(
        store,
        vmm,
        options,
        Some(cancel),
        |vmm_home, vm_id, raw_disk, disk_gib| {
            stage_local_disk_cancellable(vmm_home, vm_id, raw_disk, disk_gib, cancel)
        },
    )
}

/// Same as [`create_local_vm`] with an injectable disk stager for fast stub tests.
pub fn create_local_vm_with_disk_stager<T, F>(
    store: &LocalVmStore,
    vmm: &VmmClient<T>,
    options: &CreateLocalVmOptions,
    disk_stager: F,
) -> Result<CreateLocalVmOutput>
where
    T: VmmTransport,
    F: FnOnce(&Path, &str, &Path, u32) -> Result<PathBuf>,
{
    create_local_vm_with_disk_stager_cancellable(store, vmm, options, None, disk_stager)
}

/// Same as [`create_local_vm_with_disk_stager`] with an optional cancellation flag.
pub fn create_local_vm_with_disk_stager_cancellable<T, F>(
    store: &LocalVmStore,
    vmm: &VmmClient<T>,
    options: &CreateLocalVmOptions,
    cancel: Option<&AtomicBool>,
    disk_stager: F,
) -> Result<CreateLocalVmOutput>
where
    T: VmmTransport,
    F: FnOnce(&Path, &str, &Path, u32) -> Result<PathBuf>,
{
    let op_deadline = options.timeout.map(|t| Instant::now() + t);

    check_canceled(cancel, "validating create inputs")?;
    validate_create_options(options)?;

    // Lock order: staging lock FIRST (when `--disk-staging` is set), then per-VM lock, then
    // per-image lock. `disk prepare` and `disk clean` take only the staging lock, so this order
    // is strictly acyclic and prevents any concurrent prepare/clean from swapping `data.img`
    // between SHA-256 hashing and `qemu-img convert`.
    let (disk_source, staging_held) = resolve_disk_source(store, options, op_deadline, cancel)?;

    let compose_file = read_compose_file(options.app_compose_path.as_deref())?;
    let app_id = calc_app_id(&compose_file);
    check_canceled(cancel, "hashing raw disk image")?;
    let disk_source_sha256 = sha256_sparse_file(&disk_source)
        .wrap_err_with(|| format!("hash raw disk image {}", disk_source.display()))?;

    let guard = store.lock_vm_with_options(&options.name, op_deadline, cancel)?;

    check_canceled(cancel, "publishing local guest image")?;
    let published_locked = publish_local_image_locked_cancellable(
        &options.vmm_home,
        LocalImageSource {
            image_dir: &options.image_dir,
            image_name: options.image_name.as_deref(),
            flavor_label: &options.flavor,
        },
        op_deadline,
        cancel,
    )?;
    let published = published_locked.published;
    let image_lock = published_locked.lock;

    let inputs = ResolvedCreateInputs {
        disk_source,
        disk_source_sha256,
        compose_file,
        app_id,
        image_name: published.name.clone(),
        os_image_hash: published.digest.clone(),
    };
    let run = CreateRun {
        vmm,
        journal: VmJournal {
            store,
            guard: &guard,
        },
        options,
        inputs: &inputs,
        op_deadline,
        cancel,
    };
    let mut state = match store.load_optional_locked(&guard, &options.name)? {
        Some(prev) => run.resume(prev)?,
        None => run.begin()?,
    };
    // Release the per-image lock only after CreateVm has returned and the VM is registered on VMM
    // and persisted locally as `Created` (or higher), preventing a concurrent `image remove` from
    // deleting the image in the window between publication and VMM registration.
    drop(image_lock);

    run.provision(&mut state, disk_stager)?;
    let state_file = store.save_locked(&guard, &state)?;

    let (staging_consumed, warning) = consume_staging(store, options, staging_held, &state.name);

    Ok(CreateLocalVmOutput {
        name: state.name,
        vm_id: state.vm_id,
        owner_tag: state.owner_tag,
        image_name: state.image_name,
        os_image_hash: state.os_image_hash,
        image_reused: published.reused,
        mac: state.mac,
        bridge: state.bridge,
        intended_ip: state.intended_ip,
        vcpu: state.vcpu,
        memory_gib: state.memory_gib,
        disk_gib: state.disk_gib,
        phase: state.phase,
        staging_consumed,
        warning,
        state_file,
    })
}

fn validate_create_options(options: &CreateLocalVmOptions) -> Result<()> {
    validate_vm_name(&options.name)?;
    if let Some(tag) = &options.owner_tag {
        validate_owner_tag(tag)?;
    }
    ensure!(
        !options.consume_staging || options.disk_staging_name.is_some(),
        "--consume-staging requires --disk-staging"
    );
    if let Some(staging_name) = &options.disk_staging_name {
        validate_vm_name(staging_name)?;
    }
    validate_iface_name(&options.bridge, "bridge")?;
    validate_flavor(&options.flavor)?;
    ensure!(options.vcpu > 0, "--vcpu must be positive");
    ensure!(options.memory_gib > 0, "--memory-gib must be positive");
    ensure!(options.disk_gib > 0, "--disk-gib must be positive");
    Ok(())
}

/// Resolve the raw disk to convert: the staged disk under its held staging lock, or the plain
/// `--disk` path.
fn resolve_disk_source(
    store: &LocalVmStore,
    options: &CreateLocalVmOptions,
    op_deadline: Option<Instant>,
    cancel: Option<&AtomicBool>,
) -> Result<(PathBuf, Option<HeldStaging>)> {
    let Some(staging_name) = &options.disk_staging_name else {
        ensure!(
            options.disk_source.is_file(),
            "raw disk image does not exist: {}",
            options.disk_source.display()
        );
        return Ok((options.disk_source.clone(), None));
    };
    store.verify_staging_root()?;
    let lock = store.lock_staging_with_options(staging_name, op_deadline, cancel)?;
    let resolved = store.resolve_staged_disk(staging_name)?;
    let held = HeldStaging {
        name: staging_name.clone(),
        lock,
    };
    Ok((resolved, Some(held)))
}

/// Read and JSON-check the app-compose file, or use the default compose.
fn read_compose_file(path: Option<&Path>) -> Result<String> {
    let Some(path) = path else {
        return Ok(APP_COMPOSE_JSON.to_owned());
    };
    let content = fs::read_to_string(path)
        .wrap_err_with(|| format!("read app-compose file {}", path.display()))?;
    let _: serde_json::Value = serde_json::from_str(&content)
        .wrap_err_with(|| format!("invalid JSON in app-compose file {}", path.display()))?;
    Ok(content)
}

/// Consume the staged raw disk ONLY AFTER `LocalVmPhase::Ready` has been durably saved so any
/// failure or interruption before `Ready` preserves the staged disk for retry/resume. If
/// consumption fails after durable Ready, return a warning and `staging_consumed: false` (exit 0)
/// so callers can clean it up explicitly with `disk clean --staging-name`.
///
/// The staging lock is released when this function returns.
fn consume_staging(
    store: &LocalVmStore,
    options: &CreateLocalVmOptions,
    staging_held: Option<HeldStaging>,
    vm_name: &str,
) -> (bool, Option<String>) {
    let Some(held) = staging_held.filter(|_| options.consume_staging) else {
        return (false, None);
    };
    let staging_name = &held.name;
    match store.clean_staging_dir_locked(&held.lock, staging_name) {
        Ok(cleaned) => (cleaned.removed, None),
        Err(err) => (
            false,
            Some(format!(
                "WARNING: VM `{vm_name}` reached Ready, but failed to remove staged disk `{staging_name}` ({err:#}); run `outbe-up disk clean --staging-name {staging_name}`"
            )),
        ),
    }
}

impl<T: VmmTransport> CreateRun<'_, T> {
    /// Resume a partial state left by an earlier run with the same immutable inputs.
    fn resume(&self, mut prev: LocalVmState) -> Result<LocalVmState> {
        ensure!(
            prev.phase != LocalVmPhase::Ready,
            "VM `{}` ({}) already exists in {}; run `outbe-up vm remove --name {}` first",
            prev.name,
            prev.vm_id,
            self.journal.store.state_path(&prev.name)?.display(),
            prev.name
        );
        ensure!(
            prev.phase != LocalVmPhase::Removing,
            "VM `{}` ({}) is mid-removal; finish cleanup with `outbe-up vm remove --name {}`",
            prev.name,
            prev.vm_id,
            prev.name
        );
        ensure_immutable_create_params_match(&prev, self.options, self.inputs)?;

        if prev.phase == LocalVmPhase::Creating {
            self.resolve_creating(&mut prev)?;
        } else {
            self.verify_partial(&prev)?;
        }
        Ok(prev)
    }

    /// Adopt the VM of an interrupted `Creating` intent, or issue a new `CreateVm` when the
    /// caller allows `--retry-create`.
    fn resolve_creating(&self, prev: &mut LocalVmState) -> Result<()> {
        let poll = Poll {
            deadline: compute_grace_deadline(self.options.recovery_grace, self.op_deadline),
            interval: self.options.poll_interval,
        };
        let probe = probe_creating_intent(self.vmm, prev, poll, self.cancel)?;
        let Some(recovered) = probe.adopted else {
            return self.retry_create(prev, &probe.token);
        };
        recovered.ensure_never_started_and_stopped()?;
        prev.vm_id = recovered.id.clone();
        prev.phase = LocalVmPhase::Created;
        self.journal.record(
            prev,
            "recovered-creating",
            Some(format!("vm_id={}", recovered.id)),
        )
    }

    fn retry_create(&self, prev: &mut LocalVmState, token: &str) -> Result<()> {
        ensure!(
            self.options.retry_create,
            "VM `{}` has an unresolved `Creating` intent (token {}) and did not appear on the local VMM within {}s; an earlier CreateVm request may still be completing. Inspect the VMM, wait, or re-run with `--retry-create` to issue a new CreateVm (or `outbe-up vm remove --name {} --forget-intent` to discard the intent)",
            prev.name,
            token,
            self.options.recovery_grace.as_secs(),
            prev.name
        );
        check_canceled(self.cancel, "re-checking collisions before --retry-create")?;
        ensure_no_foreign_name_collision(self.vmm, &prev.name, self.op_deadline)?;

        let new_token = generate_create_token()?;
        prev.create_token = Some(new_token.clone());
        self.journal
            .record(prev, "retry-create", Some(format!("token={new_token}")))?;
        self.issue_and_record(prev, &new_token)
    }

    /// Check that a partial VM past `Creating` still exists on the VMM unstarted.
    fn verify_partial(&self, prev: &LocalVmState) -> Result<()> {
        check_canceled(self.cancel, "querying partial VM info")?;
        let info_resp = self
            .vmm
            .get_info_with_deadline(&prev.vm_id, self.op_deadline)?;
        ensure!(
            !info_resp.is_gone(),
            "recorded partial VM `{}` ({}) is missing from VMM; run `outbe-up vm remove --name {}` to clean up",
            prev.name,
            prev.vm_id,
            prev.name
        );
        let info = info_resp.info.unwrap();
        ensure!(
            info.name == prev.name,
            "recorded partial VM {} now has unexpected VMM name `{}`; run `outbe-up vm remove --name {}`",
            prev.vm_id,
            info.name,
            prev.name
        );
        info.ensure_never_started_and_stopped()
    }

    /// Start a fresh create: refuse name collisions, journal the `Creating` intent, then issue
    /// `CreateVm`.
    fn begin(&self) -> Result<LocalVmState> {
        check_canceled(self.cancel, "checking VMM name collisions")?;
        // Refuse name collisions with any foreign VM already registered on the local VMM.
        ensure_no_foreign_name_collision(self.vmm, &self.options.name, self.op_deadline)?;

        check_canceled(self.cancel, "recording Creating intent before CreateVm")?;
        // Journal durable `Creating` intent with a unique annotation token BEFORE CreateVm
        // so a crash or lost response never strands an unowned VM.
        let create_token = generate_create_token()?;
        let mut intent = creating_intent(self.options, self.inputs, &create_token);
        self.journal.record(
            &mut intent,
            "creating",
            Some(format!("token={create_token}")),
        )?;
        self.issue_and_record(&mut intent, &create_token)?;
        Ok(intent)
    }

    /// Issue `CreateVm` with `create_token` and journal the returned VM ID as `Created`.
    fn issue_and_record(&self, state: &mut LocalVmState, create_token: &str) -> Result<()> {
        check_canceled(self.cancel, "issuing CreateVm")?;
        let created_id = issue_create_vm(
            self.vmm,
            self.options,
            self.inputs,
            create_token,
            self.op_deadline,
        )?;
        state.vm_id = created_id.clone();
        state.phase = LocalVmPhase::Created;
        state.record_step("created", Some(format!("vm_id={created_id}")));
        persist_created_or_cleanup(self.journal, self.vmm, state)
    }

    /// Advance a created VM through MAC recording and disk staging to `Ready` (not yet saved).
    fn provision<F>(&self, state: &mut LocalVmState, disk_stager: F) -> Result<()>
    where
        F: FnOnce(&Path, &str, &Path, u32) -> Result<PathBuf>,
    {
        if state.phase == LocalVmPhase::Created {
            self.record_mac(state)?;
        }
        if state.phase == LocalVmPhase::MacRecorded {
            self.stage_disk(state, disk_stager)?;
        }
        if state.phase == LocalVmPhase::DiskStaged {
            state.phase = LocalVmPhase::Ready;
            state.record_step("ready", None);
        }
        Ok(())
    }

    fn record_mac(&self, state: &mut LocalVmState) -> Result<()> {
        check_canceled(self.cancel, "fetching VM info")?;
        self.journal.record(state, "fetching-info", None)?;

        let info_resp = self
            .vmm
            .get_info_with_deadline(&state.vm_id, self.op_deadline)?;
        ensure!(
            !info_resp.is_gone(),
            "VMM GetInfo reported VM {} not found immediately after CreateVm",
            state.vm_id
        );
        let info = info_resp.info.unwrap();
        info.ensure_never_started_and_stopped()?;
        let mac = extract_primary_mac(&info)?;

        state.mac = mac.clone();
        state.phase = LocalVmPhase::MacRecorded;
        self.journal
            .record(state, "mac-recorded", Some(format!("mac={mac}")))
    }

    fn stage_disk<F>(&self, state: &mut LocalVmState, disk_stager: F) -> Result<()>
    where
        F: FnOnce(&Path, &str, &Path, u32) -> Result<PathBuf>,
    {
        check_canceled(self.cancel, "staging hda.img")?;
        // Re-verify right before touching hda.img that no external VMM client started this VM.
        let info_resp = self
            .vmm
            .get_info_with_deadline(&state.vm_id, self.op_deadline)?;
        ensure!(
            !info_resp.is_gone(),
            "VMM GetInfo reported VM {} missing before disk staging",
            state.vm_id
        );
        let info = info_resp.info.unwrap();
        ensure!(
            info.name == state.name,
            "VMM VM {} has unexpected name `{}` (expected `{}`)",
            state.vm_id,
            info.name,
            state.name
        );
        info.ensure_never_started_and_stopped()?;

        check_canceled(self.cancel, "starting disk conversion")?;
        let detail = format!(
            "source={} sha256={}",
            state.disk_source.display(),
            self.inputs.disk_source_sha256
        );
        self.journal.record(state, "staging-disk", Some(detail))?;

        let hda_path = disk_stager(
            &state.vmm_home,
            &state.vm_id,
            &state.disk_source,
            state.disk_gib,
        )?;
        state.phase = LocalVmPhase::DiskStaged;
        self.journal.record(
            state,
            "disk-staged",
            Some(format!("hda={}", hda_path.display())),
        )
    }
}

/// Durable `Creating` intent recorded before `CreateVm`.
fn creating_intent(
    options: &CreateLocalVmOptions,
    inputs: &ResolvedCreateInputs,
    create_token: &str,
) -> LocalVmState {
    LocalVmState {
        version: LOCAL_VM_STATE_VERSION,
        name: options.name.clone(),
        vm_id: String::new(),
        create_token: Some(create_token.to_owned()),
        owner_tag: options.owner_tag.clone(),
        vmm_socket: options.vmm_socket.clone(),
        vmm_home: options.vmm_home.clone(),
        image_name: inputs.image_name.clone(),
        os_image_hash: inputs.os_image_hash.clone(),
        disk_source: inputs.disk_source.clone(),
        disk_source_sha256: Some(inputs.disk_source_sha256.clone()),
        vcpu: options.vcpu,
        memory_gib: options.memory_gib,
        disk_gib: options.disk_gib,
        bridge: options.bridge.clone(),
        intended_ip: options.intended_ip,
        mac: String::new(),
        app_id: inputs.app_id.clone(),
        no_tee: options.no_tee,
        phase: LocalVmPhase::Creating,
        journal: Vec::new(),
    }
}

fn ensure_immutable_create_params_match(
    prev: &LocalVmState,
    options: &CreateLocalVmOptions,
    inputs: &ResolvedCreateInputs,
) -> Result<()> {
    let mut mismatches = Mismatches::default();
    check_image_and_hardware(&mut mismatches, prev, options, inputs);
    check_identity_and_host(&mut mismatches, prev, options, inputs);
    ensure!(
        mismatches.is_empty(),
        "partial state for VM `{}` has mismatched immutable parameters ({}); retry with original inputs or run `outbe-up vm remove --name {}` first",
        prev.name,
        mismatches.join(),
        prev.name
    );
    Ok(())
}

/// Compare the guest image, network bridge, and VM size.
fn check_image_and_hardware(
    mismatches: &mut Mismatches,
    prev: &LocalVmState,
    options: &CreateLocalVmOptions,
    inputs: &ResolvedCreateInputs,
) {
    mismatches.check(prev.image_name != inputs.image_name, || {
        format!(
            "image_name (`{}` vs `{}`)",
            prev.image_name, inputs.image_name
        )
    });
    mismatches.check(prev.os_image_hash != inputs.os_image_hash, || {
        format!(
            "os_image_hash (`{}` vs `{}`)",
            prev.os_image_hash, inputs.os_image_hash
        )
    });
    mismatches.check(prev.bridge != options.bridge, || {
        format!("bridge (`{}` vs `{}`)", prev.bridge, options.bridge)
    });
    mismatches.check(prev.disk_gib != options.disk_gib, || {
        format!("disk_gib ({} vs {})", prev.disk_gib, options.disk_gib)
    });
    mismatches.check(prev.vcpu != options.vcpu, || {
        format!("vcpu ({} vs {})", prev.vcpu, options.vcpu)
    });
    mismatches.check(prev.memory_gib != options.memory_gib, || {
        format!("memory_gib ({} vs {})", prev.memory_gib, options.memory_gib)
    });
    mismatches.check(prev.no_tee != options.no_tee, || {
        format!("no_tee ({} vs {})", prev.no_tee, options.no_tee)
    });
}

/// Compare the app identity, ownership, addressing, VMM paths, and raw disk source.
fn check_identity_and_host(
    mismatches: &mut Mismatches,
    prev: &LocalVmState,
    options: &CreateLocalVmOptions,
    inputs: &ResolvedCreateInputs,
) {
    mismatches.check(prev.app_id != inputs.app_id, || {
        format!("app_id (`{}` vs `{}`)", prev.app_id, inputs.app_id)
    });
    mismatches.check(prev.owner_tag != options.owner_tag, || {
        format!(
            "owner_tag ({:?} vs {:?})",
            prev.owner_tag, options.owner_tag
        )
    });
    mismatches.check(prev.intended_ip != options.intended_ip, || {
        format!(
            "intended_ip ({:?} vs {:?})",
            prev.intended_ip, options.intended_ip
        )
    });
    mismatches.check(prev.vmm_socket != options.vmm_socket, || {
        format!(
            "vmm_socket (`{}` vs `{}`)",
            prev.vmm_socket.display(),
            options.vmm_socket.display()
        )
    });
    mismatches.check(prev.vmm_home != options.vmm_home, || {
        format!(
            "vmm_home (`{}` vs `{}`)",
            prev.vmm_home.display(),
            options.vmm_home.display()
        )
    });
    mismatches.check(prev.disk_source != inputs.disk_source, || {
        format!(
            "disk_source (`{}` vs `{}`)",
            prev.disk_source.display(),
            inputs.disk_source.display()
        )
    });
    let current_sha = &inputs.disk_source_sha256;
    mismatches.check(
        prev.disk_source_sha256.as_ref() != Some(current_sha),
        || match &prev.disk_source_sha256 {
            Some(recorded_sha) => {
                format!("disk_source_sha256 (`{recorded_sha}` vs `{current_sha}`)")
            }
            None => "disk_source_sha256 (missing in partial state)".to_owned(),
        },
    );
}

fn issue_create_vm<T: VmmTransport>(
    vmm: &VmmClient<T>,
    options: &CreateLocalVmOptions,
    inputs: &ResolvedCreateInputs,
    create_token: &str,
    deadline: Option<Instant>,
) -> Result<String> {
    let mut annotations = BTreeMap::new();
    annotations.insert(CREATE_TOKEN_ANNOTATION.to_owned(), create_token.to_owned());
    if let Some(owner_tag) = &options.owner_tag {
        annotations.insert(OWNER_TAG_ANNOTATION.to_owned(), owner_tag.clone());
    }
    vmm.create_vm_with_deadline(
        &CreateVmRequest {
            name: options.name.clone(),
            image: inputs.image_name.clone(),
            compose_file: inputs.compose_file.clone(),
            vcpu: options.vcpu,
            memory: options.memory_gib * 1024,
            disk_size: options.disk_gib,
            ports: Vec::new(),
            app_id: Some(inputs.app_id.clone()),
            user_config: String::new(),
            hugepages: false,
            pin_numa: false,
            stopped: true,
            no_tee: options.no_tee,
            networking: Some(NetworkingConfig {
                mode: "bridge".to_owned(),
                bridge_name: options.bridge.clone(),
            }),
            annotations,
        },
        deadline,
    )
}

/// Save the newly `Created` state, or remove the new VM from the VMM when the save fails.
fn persist_created_or_cleanup<T: VmmTransport>(
    journal: VmJournal<'_>,
    vmm: &VmmClient<T>,
    state: &LocalVmState,
) -> Result<()> {
    let Err(save_err) = journal.save(state) else {
        return Ok(());
    };
    let created_id = &state.vm_id;
    let cleanup_note = match vmm.remove_vm(created_id) {
        Ok(()) => format!("removed newly created VM {created_id}"),
        Err(rm_err) => format!(
            "failed to remove newly created VM {created_id} ({rm_err:#}); remove it manually"
        ),
    };
    bail!("failed to journal newly created VM {created_id} ({save_err:#}); {cleanup_note}");
}
