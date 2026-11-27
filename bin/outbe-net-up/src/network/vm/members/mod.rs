//! Joining-validator and keyless full-node VM orchestration (`outbe-net-up network add-validator|add-full-node`).

mod driver;
mod full_node;
mod join;
mod keys;
mod provision;

use std::{
    collections::BTreeMap,
    net::Ipv4Addr,
    path::Path,
    sync::Arc,
    time::{Duration, Instant},
};

use eyre::Result;
use outbe_operator_remote::{HostExec, SshControlSession};

use super::{
    ensure_pinned_outbe_up_on_host, ensure_vm_deployment, ImageSource, RemoteOutbeUp, UniqueHost,
};
use crate::network::remote::NetworkArtifacts;

#[cfg(test)]
pub(crate) use driver::JoinChainDriver;
pub(crate) use full_node::add_full_node_with_exec;
pub(super) use full_node::{fetch_member_serial_tail, verify_finalized_hash_match};
pub(crate) use join::add_validator_with_exec;
#[cfg(test)]
pub(crate) use keys::ensure_local_member_keys;

#[derive(Clone, Debug)]
pub struct AddValidatorOptions<'a> {
    pub artifacts: &'a Path,
    pub name: &'a str,
    pub host: &'a str,
    pub user: Option<&'a str>,
    pub port: u16,
    pub vmm_home: Option<&'a str>,
    pub bridge: Option<&'a str>,
    pub vm_ip: Ipv4Addr,
    pub rpc_bind_ip: Option<Ipv4Addr>,
    pub advertise_ip: Ipv4Addr,
    pub rpc_port: u16,
    pub port_offset: u16,
    pub image_source: Option<ImageSource>,
    pub vcpu: Option<u32>,
    pub memory_gib: Option<u32>,
    pub disk_gib: Option<u32>,
    pub no_tee: bool,
    pub ssh_key: Option<&'a Path>,
    pub ssh_config: Option<&'a Path>,
    pub timeout: Duration,
    pub max_lag: u64,
    pub min_finalized_blocks: u64,
}

#[derive(Clone, Debug)]
pub struct AddFullNodeOptions<'a> {
    pub artifacts: &'a Path,
    pub name: &'a str,
    pub host: &'a str,
    pub user: Option<&'a str>,
    pub port: u16,
    pub vmm_home: Option<&'a str>,
    pub bridge: Option<&'a str>,
    pub vm_ip: Ipv4Addr,
    pub rpc_bind_ip: Option<Ipv4Addr>,
    pub advertise_ip: Option<Ipv4Addr>,
    pub rpc_port: u16,
    pub image_source: Option<ImageSource>,
    pub vcpu: Option<u32>,
    pub memory_gib: Option<u32>,
    pub disk_gib: Option<u32>,
    pub no_tee: bool,
    pub ssh_key: Option<&'a Path>,
    pub ssh_config: Option<&'a Path>,
    pub timeout: Duration,
    pub max_lag: u64,
    pub min_finalized_blocks: u64,
}

pub async fn add_validator(options: AddValidatorOptions<'_>) -> Result<()> {
    let build = NetworkArtifacts::load(options.artifacts)?;
    ensure_vm_deployment(&build)?;
    let driver =
        driver::RealJoinChainDriver::from_artifacts(&build.root, &build.config.certified_rpc)?;
    let session = Arc::new(SshControlSession::new_for_network(
        options.ssh_key,
        options.ssh_config,
        &build.manifest.genesis.final_sha256,
    )?);
    let exec: Arc<dyn HostExec> = session.clone();
    let res = add_validator_with_exec(&options, exec, &driver).await;
    session.close_all();
    res
}

pub async fn add_full_node(options: AddFullNodeOptions<'_>) -> Result<()> {
    let build = NetworkArtifacts::load(options.artifacts)?;
    ensure_vm_deployment(&build)?;
    let session = Arc::new(SshControlSession::new_for_network(
        options.ssh_key,
        options.ssh_config,
        &build.manifest.genesis.final_sha256,
    )?);
    let exec: Arc<dyn HostExec> = session.clone();
    let res = add_full_node_with_exec(&options, exec).await;
    session.close_all();
    res
}

pub(super) use super::load_and_verify_vm_orchestration as load_and_verify_member_preflight;

pub(super) fn resolve_member_host_bins(
    exec: &dyn HostExec,
    artifacts_root: &Path,
    outbe_up_sha256: &str,
    unique_hosts: &BTreeMap<String, UniqueHost>,
    (target_hkey, target_bin): (String, RemoteOutbeUp),
) -> Result<BTreeMap<String, RemoteOutbeUp>> {
    let mut bins = BTreeMap::new();
    bins.insert(target_hkey, target_bin);
    for (hkey, host) in unique_hosts {
        if bins.contains_key(hkey) {
            continue;
        }
        let bin = ensure_pinned_outbe_up_on_host(
            exec,
            &host.ssh,
            artifacts_root,
            outbe_up_sha256,
            false,
        )?;
        bins.insert(hkey.clone(), bin);
    }
    Ok(bins)
}

pub(super) async fn sleep_until_poll_tick(deadline: Instant) {
    let remaining = deadline.saturating_duration_since(Instant::now());
    if !remaining.is_zero() {
        tokio::time::sleep(remaining.min(Duration::from_secs(5))).await;
    }
}
