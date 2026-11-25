//! CLI wrapper for building a VM data disk image from network artifacts.

use std::path::Path;

use eyre::{bail, ensure, Result};
use outbe_operator_node_role::progress::Progress;
use outbe_operator_vm::disk::build_disk_image;

use crate::network::{
    build::{DeploymentKind, FounderArtifact},
    remote::NetworkArtifacts,
};

const GIB: u64 = 1024 * 1024 * 1024;

/// Build a deterministic ext4 data disk image for one VM founder.
pub fn run(
    artifacts: &Path,
    founder_selector: &str,
    output: &Path,
    size_gib: Option<u32>,
) -> Result<()> {
    let mut progress = Progress::new();
    progress.stage("loading VM network artifacts");
    ensure!(
        !output.exists(),
        "output disk image already exists: {}",
        output.display()
    );
    let build = NetworkArtifacts::load(artifacts)?;
    ensure!(
        build.manifest.deployment == DeploymentKind::Vm,
        "network artifacts use bare-host deployment, not vm"
    );
    let founder = select_founder(&build.manifest.founders, founder_selector)?;
    let vm = founder
        .vm
        .as_ref()
        .ok_or_else(|| eyre::eyre!("founder {} manifest has no vm metadata", founder.name))?;
    let disk_gib = size_gib.unwrap_or(vm.disk_gib);
    ensure!(disk_gib > 0, "disk size must be positive");
    let disk_bytes = u64::from(disk_gib) * GIB;
    let source_tree = build.root.join(&vm.data_tree);
    ensure!(
        source_tree.is_dir(),
        "founder VM data tree does not exist: {}",
        source_tree.display()
    );

    progress.stage(&format!(
        "building {disk_gib} GiB data disk for {}",
        founder.name
    ));
    build_disk_image(&source_tree, output, disk_bytes)?;
    progress.done(&format!("wrote VM data disk to {}", output.display()));
    Ok(())
}

fn select_founder<'a>(
    founders: &'a [FounderArtifact],
    selector: &str,
) -> Result<&'a FounderArtifact> {
    if let Ok(index) = selector.parse::<usize>() {
        if let Some(founder) = founders.iter().find(|item| item.index == index) {
            return Ok(founder);
        }
    }
    if let Some(founder) = founders.iter().find(|item| item.name == selector) {
        return Ok(founder);
    }
    bail!("unknown founder '{selector}' in network manifest");
}

#[cfg(test)]
mod tests {
    use std::fs;

    use outbe_software_bundle::sha256_file;

    use super::*;

    #[test]
    fn refuses_non_vm_manifest_and_existing_output() {
        let temp = tempfile::tempdir().unwrap();
        let existing = temp.path().join("exists.img");
        fs::write(&existing, b"existing").unwrap();
        let err = run(temp.path(), "0", &existing, None).unwrap_err();
        assert!(err.to_string().contains("already exists"), "{err}");

        // Validate that a bare-host deployment kind is rejected.
        let artifacts = temp.path().join("host-artifacts");
        fs::create_dir_all(artifacts.join("dist")).unwrap();
        fs::write(
            artifacts.join("network.yaml"),
            crate::network::config::EXAMPLE,
        )
        .unwrap();
        fs::write(artifacts.join("software-bundle.tar.zst"), b"bundle").unwrap();
        let hash = alloy_primitives::B256::repeat_byte(0x42);
        let policy = outbe_primitives::tee_genesis_v1::initial_tee_policy_v1(
            outbe_primitives::tee_genesis_v1::InitialTeeProfileV1::NoAttestationDev,
            54_322_345,
            hash,
        )
        .unwrap();
        let genesis = serde_json::json!({
            "config": {
                "chainId": 54_322_345,
                "teeAttestationV1": outbe_primitives::tee_genesis_v1::tee_attestation_v1_genesis_field(&policy).unwrap()
            }
        });
        fs::write(
            artifacts.join("genesis.json"),
            serde_json::to_vec(&genesis).unwrap(),
        )
        .unwrap();
        let mut founders = Vec::new();
        for index in 0..4 {
            let rel = format!("dist/validator-{index}.tgz");
            fs::write(artifacts.join(&rel), b"tgz").unwrap();
            founders.push(serde_json::json!({
                "index": index,
                "name": format!("validator-{index}"),
                "ssh_host": format!("192.0.2.{}", index + 10),
                "advertise_ip": format!("10.0.0.{}", index + 10),
                "validator_address": "0x1111111111111111111111111111111111111111",
                "hyperlane_address": "0x2222222222222222222222222222222222222222",
                "consensus_public_key": "aa",
                "archive": rel,
                "archive_sha256": sha256_file(&artifacts.join(format!("dist/validator-{index}.tgz"))).unwrap(),
            }));
        }
        let manifest = serde_json::json!({
            "version": 1,
            "state": "ready",
            "deployment": "host",
            "name": "local-testnet",
            "network": "testnet",
            "chain_id": 54_322_345,
            "config_sha256": sha256_file(&artifacts.join("network.yaml")).unwrap(),
            "software": {
                "archive_sha256": sha256_file(&artifacts.join("software-bundle.tar.zst")).unwrap(),
                "commit": "abc",
                "release_tag": "v0",
                "target": "x86_64-unknown-linux-gnu",
            },
            "genesis": {
                "seeded_path": "genesis.seeded.json",
                "seeded_sha256": "00",
                "final_path": "genesis.json",
                "final_sha256": sha256_file(&artifacts.join("genesis.json")).unwrap(),
                "genesis_block_hash": format!("{hash:#x}"),
            },
            "founders": founders,
            "joining": {
                "bundle": "joining/bundle.tar.zst",
                "bundle_sha256": "00",
                "config_example": "joining/operator-config.example.yaml",
            }
        });
        fs::write(
            artifacts.join("network-manifest.json"),
            serde_json::to_vec(&manifest).unwrap(),
        )
        .unwrap();

        let output = temp.path().join("out.img");
        let err = run(&artifacts, "0", &output, None).unwrap_err();
        assert!(
            err.to_string().contains("bare-host deployment, not vm"),
            "{err}"
        );
    }
}
